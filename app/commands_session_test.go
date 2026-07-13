package app

import (
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestCompactCommandUsesRunner(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{}
	model.Model.Runner = runner

	updated, cmd := model.handleCompactCommand()
	if cmd == nil {
		t.Fatal("compact command is nil")
	}
	if !updated.Progress.Compacting {
		t.Fatal("compaction state is not active")
	}
	msg := cmd()
	result, ok := msg.(sessionCompactedMsg)
	if !ok {
		t.Fatalf("compact result = %T, want sessionCompactedMsg", msg)
	}
	if result.notice == "" {
		t.Fatal("compact result has no notice")
	}
	if runner.compacts != 1 {
		t.Fatalf("runner compact calls = %d, want 1", runner.compacts)
	}

	// Keep the command result compatible with Bubble Tea's message contract.
	var _ tea.Msg = result
}
