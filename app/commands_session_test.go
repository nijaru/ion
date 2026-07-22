package app

import (
	"fmt"
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
	if result.generation != model.Model.EventGeneration {
		t.Fatalf("compact result generation = %d, want %d", result.generation, model.Model.EventGeneration)
	}
	if runner.compacts != 1 {
		t.Fatalf("runner compact calls = %d, want 1", runner.compacts)
	}

	// Keep the command result compatible with Bubble Tea's message contract.
	var _ tea.Msg = result
}

func TestStaleCompactionResultCannotReleaseNewRuntimeQueue(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.Progress.Compacting = true
	model.turnReducer().QueueTurn("queued for current runtime")

	next, cmd, handled := model.dispatchAppControlMessage(sessionCompactedMsg{
		generation: 1,
		notice:     "old runtime compacted",
	})
	if !handled {
		t.Fatal("stale compaction result was not handled")
	}
	if cmd != nil {
		t.Fatal("stale compaction result returned a command")
	}
	if !next.Progress.Compacting {
		t.Fatal("stale compaction result cleared current compaction state")
	}
	if got := len(next.InFlight.QueuedTurns); got != 1 {
		t.Fatalf("queued turns = %d, want one current-runtime queue entry", got)
	}
}

func TestCompactionFailureClearsBusyState(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{compactErr: fmt.Errorf("context budget unavailable")}
	model.Model.Runner = runner

	updated, cmd := model.handleCompactCommand()
	if cmd == nil {
		t.Fatal("compact command is nil")
	}
	msg := cmd()
	result, ok := msg.(sessionCompactedMsg)
	if !ok {
		t.Fatalf("compact result = %T, want sessionCompactedMsg", msg)
	}
	if result.err == nil {
		t.Fatal("compact error = nil, want compaction failure")
	}

	next, followUp := updated.handleSessionCompacted(result)
	if followUp == nil {
		t.Fatal("compaction failure did not return an error presentation command")
	}
	if next.Progress.Compacting {
		t.Fatal("compaction failure left compacting state active")
	}
}
