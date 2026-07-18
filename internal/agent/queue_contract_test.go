package agent

import (
	"errors"
	"testing"
)

func TestHarnessInputQueuesAreBounded(t *testing.T) {
	h := NewHarness(HarnessConfig{
		Session:       newTestSession(t),
		QueueCapacity: 1,
	})
	defer h.Close()
	h.phase = PhaseTurn

	if err := h.Steer("first"); err != nil {
		t.Fatalf("first steer: %v", err)
	}
	if err := h.Steer("second"); !errors.Is(err, ErrQueueFull) {
		t.Fatalf("second steer error = %v, want ErrQueueFull", err)
	}

	if err := h.FollowUp("first"); err != nil {
		t.Fatalf("first follow-up: %v", err)
	}
	if err := h.FollowUp("second"); !errors.Is(err, ErrQueueFull) {
		t.Fatalf("second follow-up error = %v, want ErrQueueFull", err)
	}

	if err := h.NextTurn("first"); err != nil {
		t.Fatalf("first next-turn: %v", err)
	}
	if err := h.NextTurn("second"); !errors.Is(err, ErrQueueFull) {
		t.Fatalf("second next-turn error = %v, want ErrQueueFull", err)
	}
}
