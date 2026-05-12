package canto

import "testing"

func TestTurnStateFinishClearsCancelAndTools(t *testing.T) {
	state := newTurnState()
	turnID := state.start(func() {})
	state.markToolActive(turnID, "tool-call-1")

	if !state.hasActiveTool() {
		t.Fatal("turn state did not record active tool")
	}
	if !state.finish(turnID) {
		t.Fatal("finish returned false for active turn")
	}
	if state.active {
		t.Fatal("turn remained active after finish")
	}
	if state.cancel != nil {
		t.Fatal("cancel remained set after finish")
	}
	if state.hasActiveTool() {
		t.Fatal("active tools remained after finish")
	}
}

func TestTurnStateCancelActiveReturnsCancelAndClears(t *testing.T) {
	state := newTurnState()
	var canceled bool
	turnID := state.start(func() { canceled = true })
	state.markToolActive(turnID, "tool-call-1")

	cancel, active := state.cancelActive()
	if !active {
		t.Fatal("cancelActive reported inactive turn")
	}
	if cancel == nil {
		t.Fatal("cancelActive returned nil cancel")
	}
	cancel()
	if !canceled {
		t.Fatal("returned cancel func did not run")
	}
	if state.active {
		t.Fatal("turn remained active after cancelActive")
	}
	if state.cancel != nil {
		t.Fatal("cancel remained set after cancelActive")
	}
	if state.hasActiveTool() {
		t.Fatal("active tools remained after cancelActive")
	}
}

func TestTurnStateIgnoresStaleToolMutations(t *testing.T) {
	state := newTurnState()
	first := state.start(func() {})
	second := state.start(func() {})

	state.markToolActive(first, "stale-tool")
	if state.hasActiveTool() {
		t.Fatal("stale turn marked a tool active")
	}

	state.markToolActive(second, "active-tool")
	if !state.hasActiveTool() {
		t.Fatal("active turn did not mark tool active")
	}

	state.markToolComplete(first, "active-tool")
	if !state.hasActiveTool() {
		t.Fatal("stale turn completed current tool")
	}
}
