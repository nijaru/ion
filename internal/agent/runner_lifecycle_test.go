package agent

import (
	"context"
	"testing"

	"github.com/nijaru/ion/session"
)

func TestRunnerMutationsRejectAfterClose(t *testing.T) {
	store := newTestStore(t)
	h := NewController(ControllerConfig{Session: session.NewSession(store, 64), Store: store})
	if err := h.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	ctx := context.Background()
	if _, err := h.Prompt(ctx, "late"); err == nil {
		t.Fatal("prompt after close succeeded")
	}
	if _, err := h.AppendSessionInfo(ctx, "late"); err == nil {
		t.Fatal("session info append after close succeeded")
	}
	if _, err := h.NavigateTree(ctx, "missing", NavigateOptions{}); err == nil {
		t.Fatal("tree navigation after close succeeded")
	}
	if _, err := h.AppendLabel(ctx, "missing", "late"); err == nil {
		t.Fatal("label append after close succeeded")
	}
	if _, err := h.GetLabel(ctx, "missing"); err == nil {
		t.Fatal("label read after close succeeded")
	}
	h.NextTurn("late")
	if len(h.nextTurn) != 0 {
		t.Fatal("next-turn queue changed after close")
	}
}
