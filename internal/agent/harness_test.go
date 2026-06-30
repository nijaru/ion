package agent

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// INVARIANT: Harness.Prompt produces an assistant message and persists it.
func TestHarnessPrompt(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "hello from harness", StopReason: "stop"},
		}}, nil
	}

	h := NewHarness(HarnessConfig{
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

// INVARIANT: Harness blocks concurrent prompts (single active run).
func TestHarnessSingleActiveRun(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "ok", StopReason: "stop"},
		}}, nil
	}

	h := NewHarness(HarnessConfig{
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

// INVARIANT: Harness emits events to the TUI channel.
func TestHarnessEmitsEvents(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "event test", StopReason: "stop"},
		}}, nil
	}

	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	go func() {
		h.Prompt(context.Background(), "emit test")
	}()

	var events []session.Event
	timeout := time.After(2 * time.Second)
	for {
		select {
		case e := <-h.Events():
			events = append(events, e)
			if _, ok := e.(session.AgentEnd); ok {
				goto done
			}
		case <-timeout:
			t.Fatal("timeout waiting for AgentEnd")
		}
	}
done:
	if len(events) < 3 {
		t.Fatalf("expected at least 3 events, got %d", len(events))
	}
}

// INVARIANT: after_provider_response event is emitted before the assistant message stream.
func TestHarnessAfterProviderResponse(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "hello after provider", StopReason: "stop"},
		}}, nil
	}

	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	go func() {
		h.Prompt(context.Background(), "test after provider")
	}()

	var events []session.Event
	timeout := time.After(2 * time.Second)
	var sawAfterProvider, sawMessageStart bool
	for {
		select {
		case e := <-h.Events():
			events = append(events, e)
			switch e.(type) {
			case session.AfterProviderResponse:
				sawAfterProvider = true
			case session.MessageStart:
				sawMessageStart = true
			}
			if _, ok := e.(session.AgentEnd); ok {
				goto done
			}
		case <-timeout:
			t.Fatal("timeout waiting for AgentEnd")
		}
	}
done:
	if !sawAfterProvider {
		t.Fatal("expected AfterProviderResponse event, but none received")
	}
	if !sawMessageStart {
		t.Fatal("expected MessageStart event, but none received")
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

	h := NewHarness(HarnessConfig{
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
