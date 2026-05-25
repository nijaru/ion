package canto

import "testing"

func TestTurnStateFinishClearsCancel(t *testing.T) {
	state := newTurnState()
	turnID := state.start(func() {})
	if !state.finish(turnID) {
		t.Fatal("finish returned false for active turn")
	}
	if state.active {
		t.Fatal("turn remained active after finish")
	}
	if state.cancel != nil {
		t.Fatal("cancel remained set after finish")
	}
}

func TestTurnStateTracksAcceptedCantoTurnUntilFinalPayload(t *testing.T) {
	state := newTurnState()
	turnID := state.start(func() {})
	if !state.accept(turnID, "canto-turn") {
		t.Fatal("accept returned false for active turn")
	}

	if got := state.cantoIDFor(turnID); got != "canto-turn" {
		t.Fatalf("canto id = %q, want canto-turn", got)
	}
	if got := state.cantoIDFor(turnID + 1); got != "" {
		t.Fatalf("stale turn canto id = %q, want empty", got)
	}
	if !state.finish(turnID) {
		t.Fatal("final payload finish did not clear accepted Canto turn")
	}
	if state.active || state.cancel != nil {
		t.Fatalf("turn state not cleared after final payload: %#v", state)
	}
}

func TestTurnStateRequestCancelKeepsTurnActiveUntilSettlement(t *testing.T) {
	state := newTurnState()
	var canceled bool
	turnID := state.start(func() { canceled = true })
	if !state.accept(turnID, "canto-turn") {
		t.Fatal("accept returned false for active turn")
	}

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
	if !state.active {
		t.Fatal("turn became inactive before cancel settlement")
	}
	if !state.activeFor(turnID) {
		t.Fatal("canceling turn stopped blocking new submissions before settlement")
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
	cancel, active = state.requestCancel()
	if !active {
		t.Fatal("second requestCancel reported inactive turn before settlement")
	}
	if cancel != nil {
		t.Fatal("second requestCancel returned a duplicate cancel func")
	}
	if !state.finish(turnID) {
		t.Fatal("finish returned false for current canceling turn")
	}
	if state.active || state.isCanceling(turnID) {
		t.Fatal("turn state remained canceling after settlement")
	}
}

func TestTurnStateRequestCancelBeforeAcceptanceUnblocksImmediately(t *testing.T) {
	state := newTurnState()
	turnID := state.start(func() {})

	cancel, active := state.requestCancel()
	if !active {
		t.Fatal("requestCancel reported inactive turn")
	}
	if cancel == nil {
		t.Fatal("requestCancel returned nil cancel")
	}
	if state.active {
		t.Fatal("pre-accept cancel left turn active")
	}
	if !state.isCanceling(turnID) {
		t.Fatal("pre-accept cancel did not track canceled turn")
	}
	if !state.finish(turnID) {
		t.Fatal("finish returned false for pre-accept canceled turn")
	}
	if state.isCanceling(turnID) {
		t.Fatal("pre-accept cancel marker remained after finish")
	}
}

func TestTurnStateSuppressesCanceledSettlementAfterNextTurnStarts(t *testing.T) {
	state := newTurnState()
	canceledTurn := state.start(func() {})
	state.accept(canceledTurn, "canto-turn")

	state.requestCancel()
	nextTurn := state.start(func() {})

	if state.finish(canceledTurn) {
		t.Fatal("stale canceled turn claimed terminal finish after next turn started")
	}
	if !state.activeFor(nextTurn) {
		t.Fatal("next turn was not kept active")
	}
}
