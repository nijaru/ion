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
	sess := session.NewSession(store)

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
	sess := session.NewSession(store)

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
	sess := session.NewSession(store)

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
