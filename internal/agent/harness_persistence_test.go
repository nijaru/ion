package agent

import (
	"context"
	"errors"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type failingPersistenceSession struct {
	session.Session
	messageCalls atomic.Int32
	failMessage  int32
	failModel    atomic.Bool
}

func (s *failingPersistenceSession) AppendMessage(ctx context.Context, msg session.Message) (string, error) {
	call := s.messageCalls.Add(1)
	if s.failMessage > 0 && call == s.failMessage {
		return "", errors.New("injected message persistence failure")
	}
	return s.Session.AppendMessage(ctx, msg)
}

func (s *failingPersistenceSession) AppendModelChange(ctx context.Context, provider, modelID string) (string, error) {
	if s.failModel.Load() {
		return "", errors.New("injected model persistence failure")
	}
	return s.Session.AppendModelChange(ctx, provider, modelID)
}

func TestHarnessPersistenceFailureIsTerminalAndNotAcknowledged(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	failing := &failingPersistenceSession{Session: base, failMessage: 2}
	var providerCalls atomic.Int32
	h := NewHarness(HarnessConfig{
		Session: failing,
		Store:   store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			providerCalls.Add(1)
			return &mockStream{chunks: []*llm.Chunk{{Content: "assistant", StopReason: "stop"}}}, nil
		},
	})
	var terminalEvents atomic.Int32
	var messageEnds atomic.Int32
	h.Subscribe(func(event session.Event) {
		switch event.(type) {
		case session.AgentEnd, session.Settled, session.SavePoint:
			terminalEvents.Add(1)
		case session.MessageEnd:
			messageEnds.Add(1)
		}
	})

	msg, err := h.Prompt(context.Background(), "persist this")
	if err == nil || !strings.Contains(err.Error(), "persist message") {
		t.Fatalf("Prompt error = %v, want message persistence failure", err)
	}
	if msg != nil {
		t.Fatalf("Prompt message = %#v, want nil on non-durable turn", msg)
	}
	if providerCalls.Load() != 1 {
		t.Fatalf("provider calls = %d, want one call before assistant append failure", providerCalls.Load())
	}

	snap, buildErr := failing.BuildContext(context.Background())
	if buildErr != nil {
		t.Fatal(buildErr)
	}
	if len(snap.Messages) != 1 {
		t.Fatalf("replayed messages = %d, want only durable user message", len(snap.Messages))
	}
	if text := session.MessageText(snap.Messages[0]); text != "persist this" {
		t.Fatalf("replayed message = %q, want user prompt", text)
	}
	if closeErr := h.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	if terminalEvents.Load() != 0 {
		t.Fatalf("terminal lifecycle events = %d, want none", terminalEvents.Load())
	}
	if messageEnds.Load() != 1 {
		t.Fatalf("durable MessageEnd events = %d, want one user message", messageEnds.Load())
	}
}

func TestHarnessFailedPendingWriteIsRetainedForRetry(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	failing := &failingPersistenceSession{Session: base}
	failing.failModel.Store(true)
	h := NewHarness(HarnessConfig{
		Session: failing,
		Store:   store,
		Model:   llm.Model{ID: "initial"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "assistant", StopReason: "stop"}}}, nil
		},
	})

	h.SetModel(llm.Model{ID: "next"})
	_, err := h.Prompt(context.Background(), "write model change")
	if err == nil || !strings.Contains(err.Error(), "flush pending write") {
		t.Fatalf("Prompt error = %v, want pending write failure", err)
	}

	failing.failModel.Store(false)
	if err := h.flushPending(context.Background()); err != nil {
		t.Fatalf("retry pending write: %v", err)
	}
	snap, err := failing.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if snap.ActiveModel != "next" {
		t.Fatalf("active model = %q, want retried model change", snap.ActiveModel)
	}
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
}
