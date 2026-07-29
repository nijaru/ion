package agent

import (
	"context"
	"errors"
	"testing"

	"github.com/nijaru/ion/session"
)

func TestRejectQueuedBranchLabelCommand(t *testing.T) {
	reply := make(chan SessionInfoResult, 1)
	(&Controller{}).rejectCommand(&GetBranchLabelCmd{Reply: reply})
	result := <-reply
	if !errors.Is(result.Err, ErrRuntimeClosed) {
		t.Fatalf("queued branch label error = %v, want runtime closed", result.Err)
	}
}

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
	if _, err := h.AppendSessionInfo(ctx, "missing-leaf", "late"); err == nil {
		t.Fatal("session info append after close succeeded")
	}
	if _, err := h.NavigateTree(ctx, "missing", NavigateOptions{}); err == nil {
		t.Fatal("tree navigation after close succeeded")
	}
	if _, err := h.AppendLabel(ctx, "missing-leaf", "missing", "late"); err == nil {
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
