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

func TestTurnStateRequestCancelTracksSettlementWithoutBlockingNextTurn(t *testing.T) {
	state := newTurnState()
	var canceled bool
	turnID := state.start(func() { canceled = true })
	state.markToolActive(turnID, "tool-call-1")

	cancel, active := state.requestCancel()
	if !active {
		t.Fatal("requestCancel reported inactive turn")
	}
	if cancel == nil {
		t.Fatal("requestCancel returned nil cancel")
	}
	cancel()
	if !canceled {
		t.Fatal("returned cancel func did not run")
	}
	if state.active {
		t.Fatal("turn remained active after cancel request")
	}
	if !state.isCanceling(turnID) {
		t.Fatal("turn was not marked canceling")
	}
	if !state.accepts(turnID) {
		t.Fatal("canceling turn no longer accepted settlement events")
	}
	if state.cancel != nil {
		t.Fatal("cancel remained set after cancelActive")
	}
	if state.hasActiveTool() {
		t.Fatal("active tools remained after cancelActive")
	}
	if !state.finish(turnID) {
		t.Fatal("finish returned false for current canceling turn")
	}
	if state.active || state.isCanceling(turnID) {
		t.Fatal("turn state remained canceling after settlement")
	}
}

func TestTurnStateSuppressesCanceledSettlementAfterNextTurnStarts(t *testing.T) {
	state := newTurnState()
	canceledTurn := state.start(func() {})

	state.requestCancel()
	nextTurn := state.start(func() {})

	if state.finish(canceledTurn) {
		t.Fatal("stale canceled turn claimed terminal finish after next turn started")
	}
	if !state.activeFor(nextTurn) {
		t.Fatal("next turn was not kept active")
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
