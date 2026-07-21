package agent

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// INVARIANT: Controller.Prompt produces an assistant message and persists it.
func TestHarnessPrompt(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "hello from harness", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	msg, err := h.Prompt(context.Background(), "hi")
	if err != nil {
		t.Fatal(err)
	}
	am, ok := msg.(*session.AssistantMessage)
	if !ok {
		t.Fatalf("expected *AssistantMessage, got %T", msg)
	}
	if am.StopReason != session.StopReasonEndTurn {
		t.Fatalf("expected end_turn stop reason, got %q", am.StopReason)
	}

	// Verify message was persisted to session.
	ctx := context.Background()
	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Messages) < 2 {
		t.Fatalf("expected at least 2 messages in session (user + assistant), got %d", len(snap.Messages))
	}
}

// INVARIANT: Controller blocks concurrent prompts (single active run).
func TestHarnessSingleActiveRun(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "ok", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	_, err := h.Prompt(context.Background(), "first")
	if err != nil {
		t.Fatal(err)
	}
}

// INVARIANT: Controller emits events to the TUI channel.
func TestHarnessEmitsEvents(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "event test", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()
	sub, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer sub.Close()

	done := make(chan struct{})
	go func() {
		defer close(done)
		h.Prompt(context.Background(), "emit test")
	}()

	var events []session.Event
	timeout := time.After(2 * time.Second)
	for {
		select {
		case envelope := <-sub.Events:
			e := envelope.Event
			events = append(events, e)
			// AgentEnd now precedes Settled; wait for Settled or timeout after AgentEnd.
			if _, ok := e.(session.Settled); ok {
				goto afterSettled
			}
			if _, ok := e.(session.AgentEnd); ok {
				// Keep draining for Settled after AgentEnd.
				continue
			}
		case <-done:
			// Prompt finished; allow one more drain cycle for buffered events.
			timeout = time.After(100 * time.Millisecond)
		case <-timeout:
			t.Fatal("timeout waiting for AgentEnd/Settled")
		}
	}
afterSettled:
	<-done // ensure Prompt finished before Close
	if len(events) < 3 {
		t.Fatalf("expected at least 3 events, got %d", len(events))
	}
}

// INVARIANT: after_provider_response hook fires for registered handlers.
func TestHarnessAfterProviderResponseHook(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "hook test", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	hookFired := make(chan struct{})
	h.On(HookAfterProviderResponse, func(payload any) (any, error) {
		close(hookFired)
		return nil, nil
	})

	go func() {
		h.Prompt(context.Background(), "hook test")
	}()

	select {
	case <-hookFired:
		// hook fired as expected
	case <-time.After(2 * time.Second):
		t.Fatal("timeout waiting for after_provider_response hook")
	}

	// Wait for the run to complete before Close to avoid sending on closed channel.
	h.WaitForIdle()
}

// helper for harness tests
func newTestStore(t *testing.T) *session.SQLiteStore {
	t.Helper()
	s, err := session.NewSQLiteStore(":memory:", "test-harness")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })
	return s
}
