package agent

import (
	"context"
	"errors"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestControllerSessionProjectionCapabilities(t *testing.T) {
	store := newTestStore(t)
	controller := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
	})
	t.Cleanup(func() {
		if err := controller.Close(); err != nil {
			t.Errorf("close controller: %v", err)
		}
	})

	ctx := t.Context()
	if err := controller.AppendMessage(ctx, session.NewUserText("hello", time.Now())); err != nil {
		t.Fatalf("append user message: %v", err)
	}
	branch, err := controller.SessionBranch(ctx)
	if err != nil {
		t.Fatalf("session branch: %v", err)
	}
	if len(branch) != 1 || session.EntryText(branch[0]) != "hello" {
		t.Fatalf("branch = %#v, want one hello entry", branch)
	}

	tree, err := controller.SessionTree(ctx)
	if err != nil {
		t.Fatalf("session tree: %v", err)
	}
	if tree.LeafID == "" || len(tree.Entries) != 1 || tree.Entries[0].ID() != tree.LeafID {
		t.Fatalf("tree = %#v, want one entry selected as leaf", tree)
	}
	assistant := &session.AssistantMessage{
		Content:   []session.Content{session.TextContent{Text: "answer"}},
		Usage:     session.Usage{Input: 3, Output: 5, TotalTokens: 8, Cost: session.Cost{Total: 0.25}},
		Timestamp: time.Now(),
	}
	if err := controller.AppendMessage(ctx, assistant); err != nil {
		t.Fatalf("append assistant message: %v", err)
	}

	projection, err := controller.SessionProjection(ctx)
	if err != nil {
		t.Fatalf("session projection: %v", err)
	}
	if projection.ID != store.Meta().ID {
		t.Fatalf("projection session id = %q, want %q", projection.ID, store.Meta().ID)
	}
	if projection.LeafID == "" || projection.LeafID != projection.Branch[len(projection.Branch)-1].ID() {
		t.Fatalf("projection leaf = %q, branch = %#v", projection.LeafID, projection.Branch)
	}
	if len(projection.Branch) != 2 || session.EntryText(projection.Branch[1]) != "answer" {
		t.Fatalf("projection branch = %#v, want user and assistant entries", projection.Branch)
	}
	if projection.Usage.Input != 3 || projection.Usage.Output != 5 || projection.Usage.TotalTokens != 8 || projection.Usage.Cost.Total != 0.25 {
		t.Fatalf("projection usage = %#v, want assistant usage", projection.Usage)
	}
	if controller.SessionID() != store.Meta().ID {
		t.Fatalf("session id = %q, want %q", controller.SessionID(), store.Meta().ID)
	}

	info := session.SessionInfoEntry{
		EntryBase:   session.EntryBase{ID: controller.SessionID(), Timestamp: time.Now()},
		Workdir:     "/tmp/ion-projection",
		Model:       "test/model",
		Name:        "hello",
		LastPreview: "hello",
		UpdatedAt:   time.Now(),
	}
	if err := controller.UpdateSession(ctx, info); err != nil {
		t.Fatalf("update session catalog: %v", err)
	}
	got, err := controller.GetSessionInfo(ctx, controller.SessionID())
	if err != nil {
		t.Fatalf("get session catalog: %v", err)
	}
	if got.Model != info.Model || got.LastPreview != info.LastPreview {
		t.Fatalf("catalog entry = %#v, want %#v", got, info)
	}
	sessions, err := controller.ListSessions(ctx, info.Workdir)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 || sessions[0].ID() != controller.SessionID() {
		t.Fatalf("sessions = %#v, want current session", sessions)
	}

	if err := controller.AddInput(ctx, info.Workdir, "hello"); err != nil {
		t.Fatalf("add input history: %v", err)
	}
	inputs, err := controller.GetInputs(ctx, info.Workdir, 10)
	if err != nil {
		t.Fatalf("get input history: %v", err)
	}
	if len(inputs) != 1 || inputs[0] != "hello" {
		t.Fatalf("inputs = %#v, want hello", inputs)
	}
}

func TestControllerSessionProjectionRejectsClosedAndCanceledQueries(t *testing.T) {
	store := newTestStore(t)
	controller := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
	})

	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := controller.SessionBranch(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled branch error = %v, want context canceled", err)
	}
	if _, err := controller.SessionProjection(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled projection error = %v, want context canceled", err)
	}

	if err := controller.Close(); err != nil {
		t.Fatalf("close controller: %v", err)
	}
	if _, err := controller.SessionProjection(t.Context()); !errors.Is(err, ErrRuntimeClosed) {
		t.Fatalf("closed projection error = %v, want runtime closed", err)
	}
	if _, err := controller.SessionTree(t.Context()); !errors.Is(err, ErrRuntimeClosed) {
		t.Fatalf("closed tree error = %v, want runtime closed", err)
	}
	if err := controller.AddInput(t.Context(), "/tmp", "late"); !errors.Is(err, ErrRuntimeClosed) {
		t.Fatalf("closed input history error = %v, want runtime closed", err)
	}
}

type blockingProjectionStream struct {
	ctx     context.Context
	started chan<- struct{}
	once    sync.Once
}

func (s *blockingProjectionStream) Next() (*llm.Chunk, bool) {
	s.once.Do(func() { close(s.started) })
	<-s.ctx.Done()
	return nil, false
}

func (s *blockingProjectionStream) Err() error   { return nil }
func (s *blockingProjectionStream) Close() error { return nil }

func TestControllerSessionProjectionIncludesActiveDurableTurn(t *testing.T) {
	path := filepath.Join(t.TempDir(), "projection.db")
	store, err := session.NewSQLiteStore(path, "durable-projection")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 0)
	started := make(chan struct{})
	controller := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			return &blockingProjectionStream{ctx: ctx, started: started}, nil
		},
	})

	promptDone := make(chan error, 1)
	go func() {
		_, promptErr := controller.Prompt(t.Context(), "staged prompt")
		promptDone <- promptErr
	}()
	select {
	case <-started:
	case <-time.After(2 * time.Second):
		t.Fatal("timeout waiting for provider stream")
	}

	projection, err := controller.SessionProjection(t.Context())
	if err != nil {
		t.Fatalf("active session projection: %v", err)
	}
	if len(projection.Branch) != 1 || session.EntryText(projection.Branch[0]) != "staged prompt" {
		t.Fatalf("active projection branch = %#v, want staged prompt", projection.Branch)
	}
	if projection.Usage != (session.Usage{}) {
		t.Fatalf("active projection usage = %#v, want zero before assistant response", projection.Usage)
	}
	if projection.LeafID != projection.Branch[0].ID() {
		t.Fatalf("active projection leaf = %q, want staged entry %q", projection.LeafID, projection.Branch[0].ID())
	}

	if _, _, err := controller.Abort(); err != nil {
		t.Fatalf("abort active turn: %v", err)
	}
	select {
	case <-promptDone:
	case <-time.After(2 * time.Second):
		t.Fatal("timeout waiting for aborted prompt")
	}
	if err := controller.Close(); err != nil {
		t.Fatalf("close controller: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close store: %v", err)
	}
}

func TestControllerSessionProjectionAfterDurableTurnSettlement(t *testing.T) {
	store, err := session.NewSQLiteStore(filepath.Join(t.TempDir(), "settled.db"), "settled-projection")
	if err != nil {
		t.Fatal(err)
	}
	controller := NewController(ControllerConfig{
		Session: session.NewSession(store, 0),
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{
				Content:    "settled response",
				StopReason: "stop",
				Usage:      &llm.Usage{InputTokens: 13, OutputTokens: 5, TotalTokens: 18},
			}}}, nil
		},
	})
	if _, err := controller.Prompt(t.Context(), "settle this turn"); err != nil {
		t.Fatalf("prompt: %v", err)
	}

	projection, err := controller.SessionProjection(t.Context())
	if err != nil {
		t.Fatalf("settled session projection: %v", err)
	}
	if len(projection.Branch) != 2 || session.EntryText(projection.Branch[1]) != "settled response" {
		t.Fatalf("settled projection branch = %#v, want prompt and response", projection.Branch)
	}
	if projection.Usage.Input != 13 || projection.Usage.Output != 5 || projection.Usage.TotalTokens != 18 {
		t.Fatalf("settled projection usage = %#v, want persisted response usage", projection.Usage)
	}
	if err := controller.Close(); err != nil {
		t.Fatalf("close controller: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close store: %v", err)
	}
}
