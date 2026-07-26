package agent

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// failingDurableStore keeps the real transactional turn implementation while
// injecting one append failure at the storage boundary.
type failingDurableStore struct {
	*session.SQLiteStore
	appendCalls  atomic.Int32
	failAt       int32
	blockAt      int32
	blockStart   chan struct{}
	blockRelease chan struct{}
}

func (s *failingDurableStore) AppendTurnEntry(ctx context.Context, turnID string, entry session.Entry) (string, error) {
	call := s.appendCalls.Add(1)
	if call == s.failAt {
		return "", errors.New("injected durable append failure")
	}
	if call == s.blockAt {
		if s.blockStart != nil {
			close(s.blockStart)
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-s.blockRelease:
		}
	}
	return s.SQLiteStore.AppendTurnEntry(ctx, turnID, entry)
}

func TestRuntimeDurableMessageEndIsPersistedBeforePublication(t *testing.T) {
	path := filepath.Join(t.TempDir(), "runtime-io.db")
	base, err := session.NewSQLiteStore(path, "runtime-io")
	if err != nil {
		t.Fatal(err)
	}
	store := &failingDurableStore{SQLiteStore: base, failAt: -1}
	sess := session.NewSession(store, 0)
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "durable", StopReason: "stop"}}}, nil
		},
	})

	seen := make(chan bool, 1)
	watchEvents(t, h, func(event session.Event) {
		messageEnd, ok := event.(session.MessageEnd)
		if !ok {
			return
		}
		if _, ok := messageEnd.Message.(*session.AssistantMessage); !ok {
			return
		}
		h.mu.Lock()
		turnID := h.activeTurnID
		h.mu.Unlock()
		entries, err := store.TurnBranch(context.Background(), turnID)
		if err != nil {
			seen <- false
			return
		}
		for _, entry := range entries {
			if message, ok := entry.(*session.MessageEntry); ok {
				if assistant, ok := message.Message.(*session.AssistantMessage); ok &&
					session.MessageText(assistant) == "durable" {
					if message.When().IsZero() {
						seen <- false
						return
					}
					seen <- true
					return
				}
			}
		}
		seen <- false
	})

	if _, err := h.Prompt(context.Background(), "persist"); err != nil {
		t.Fatal(err)
	}
	if persisted := <-seen; !persisted {
		t.Fatal("assistant MessageEnd was published before its durable turn entry")
	}
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestRuntimeRequireDurableRejectsEphemeralTurn(t *testing.T) {
	h := NewController(ControllerConfig{
		Session:        newTestSession(t),
		Model:          llm.Model{ID: "test"},
		RequireDurable: true,
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			t.Fatal("provider called before durable turn was established")
			return nil, nil
		},
	})
	defer h.Close()
	if _, err := h.Prompt(
		context.Background(),
		"must be durable",
	); err == nil ||
		!strings.Contains(err.Error(), "DurableStore") {
		t.Fatalf("Prompt error = %v, want missing DurableStore", err)
	}
}

func TestRuntimeCloseCancelsDurablePersistence(t *testing.T) {
	path := filepath.Join(t.TempDir(), "runtime-close.db")
	base, err := session.NewSQLiteStore(path, "runtime-close")
	if err != nil {
		t.Fatal(err)
	}
	store := &failingDurableStore{
		SQLiteStore:  base,
		failAt:       -1,
		blockAt:      2,
		blockStart:   make(chan struct{}),
		blockRelease: make(chan struct{}),
	}
	sess := session.NewSession(store, 0)
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "blocked", StopReason: "stop"}}}, nil
		},
	})
	promptDone := make(chan struct{})
	go func() {
		_, _ = h.Prompt(context.Background(), "close while persisting")
		close(promptDone)
	}()
	select {
	case <-store.blockStart:
	case <-time.After(time.Second):
		t.Fatal("durable append did not block")
	}
	closeDone := make(chan error, 1)
	go func() { closeDone <- h.Close() }()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not cancel durable persistence")
	}
	select {
	case <-promptDone:
	case <-time.After(time.Second):
		t.Fatal("prompt did not finish after Close")
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestRuntimeAbortedTurnCarriesPendingMutationForward(t *testing.T) {
	path := filepath.Join(t.TempDir(), "runtime-pending.db")
	store, err := session.NewSQLiteStore(path, "runtime-pending")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 0)
	started := make(chan struct{})
	var calls atomic.Int32
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "initial"},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			if calls.Add(1) == 1 {
				close(started)
				<-ctx.Done()
				return nil, ctx.Err()
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "recovered", StopReason: "stop"}}}, nil
		},
	})
	firstDone := make(chan struct{})
	go func() {
		_, _ = h.Prompt(context.Background(), "abort before mutation commit")
		close(firstDone)
	}()
	<-started
	if err := h.SetModel(llm.Model{ID: "next"}); err != nil {
		t.Fatal(err)
	}
	if _, _, err := h.Abort(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-firstDone:
	case <-time.After(time.Second):
		t.Fatal("aborted prompt did not finish")
	}
	if _, err := h.Prompt(context.Background(), "retry mutation"); err != nil {
		t.Fatal(err)
	}
	snapshot, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.ActiveModel != "next" {
		t.Fatalf("active model after carried-forward mutation = %q, want next", snapshot.ActiveModel)
	}
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestRuntimeDurableAppendFailureAbortsWithoutTerminalPublication(t *testing.T) {
	path := filepath.Join(t.TempDir(), "runtime-failure.db")
	base, err := session.NewSQLiteStore(path, "runtime-failure")
	if err != nil {
		t.Fatal(err)
	}
	store := &failingDurableStore{SQLiteStore: base, failAt: 2}
	sess := session.NewSession(store, 0)
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "not durable", StopReason: "stop"}}}, nil
		},
	})
	var terminal atomic.Int32
	watchEvents(t, h, func(event session.Event) {
		switch event.(type) {
		case session.AgentEnd, session.Settled:
			terminal.Add(1)
		}
	})

	_, err = h.Prompt(context.Background(), "fail assistant append")
	if err == nil || !strings.Contains(err.Error(), "injected durable append failure") {
		t.Fatalf("Prompt error = %v, want injected durable append failure", err)
	}
	if got := store.appendCalls.Load(); got != 2 {
		t.Fatalf("durable append calls = %d, want user plus failed assistant append", got)
	}
	if got := terminal.Load(); got != 0 {
		t.Fatalf("terminal events after durable append failure = %d, want 0", got)
	}
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := session.NewSQLiteStore(path, "runtime-failure")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	snapshot, err := session.NewSession(reopened, 0).BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(snapshot.Messages) != 0 {
		t.Fatalf("aborted durable messages replayed = %#v, want none", snapshot.Messages)
	}
}

func TestRuntimeAbortPublishesSettled(t *testing.T) {
	started := make(chan struct{})
	h := NewController(ControllerConfig{
		Session: newTestSession(t),
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			close(started)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	})
	defer h.Close()
	events := make(chan session.Event, 32)
	unsub := watchEvents(t, h, func(event session.Event) { events <- event })
	defer unsub()
	promptDone := make(chan struct{})
	go func() {
		_, _ = h.Prompt(context.Background(), "cancel")
		close(promptDone)
	}()
	<-started
	if _, _, err := h.Abort(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-promptDone:
	case <-time.After(time.Second):
		t.Fatal("prompt did not settle")
	}
	settled := false
	var seen []session.Event
	for {
		select {
		case event := <-events:
			seen = append(seen, event)
			if _, ok := event.(session.Settled); ok {
				settled = true
			}
		case <-time.After(time.Second):
			if !settled {
				t.Fatalf("abort did not publish Settled; events=%v detail=%#v", eventNames(seen), seen)
			}
			return
		}
		if settled {
			return
		}
	}
}
