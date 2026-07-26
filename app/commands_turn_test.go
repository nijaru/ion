package app

import (
	"context"
	"errors"
	"testing"

	"github.com/nijaru/ion/session"
)

type turnRecoveryTestRunner struct {
	stubRunner
	turn session.TurnRecord
	err  error
	ctx  context.Context
}

func (r *turnRecoveryTestRunner) InterruptedTurns(context.Context) ([]session.TurnRecord, error) {
	return []session.TurnRecord{r.turn}, nil
}

func (r *turnRecoveryTestRunner) AbortInterruptedTurn(ctx context.Context, id, _ string) (session.TurnRecord, error) {
	r.ctx = ctx
	if r.err != nil {
		return session.TurnRecord{}, r.err
	}
	if id != r.turn.ID {
		return session.TurnRecord{}, errors.New("unexpected turn ID")
	}
	r.turn.State = session.TurnAborted
	return r.turn, nil
}

func TestTurnsCommandDiscardsInterruptedTurnThroughRuntime(t *testing.T) {
	model := readyModel(t)
	expectedContext := model.Model.runtimeContext
	runner := &turnRecoveryTestRunner{turn: session.TurnRecord{
		ID: "turn-1", State: session.TurnInterrupted, Input: "draft after restart",
	}}
	model.Model.Runner = runner
	model.Model.InterruptedTurns = []session.TurnRecord{runner.turn}

	model, cmd := model.handleCommand("/turns abort turn-1")
	if cmd == nil || model.Model.InterruptedTurnRequest != 1 {
		t.Fatalf("accepted discard = request %d, cmd %v", model.Model.InterruptedTurnRequest, cmd != nil)
	}
	msg := cmd()
	model, _ = model.update(msg)
	if model.Model.InterruptedTurnRequest != 0 || len(model.Model.InterruptedTurns) != 0 {
		t.Fatalf(
			"post-discard projection = request %d turns %#v",
			model.Model.InterruptedTurnRequest,
			model.Model.InterruptedTurns,
		)
	}
	if runner.ctx != expectedContext {
		t.Fatal("discard did not receive the runtime operation context")
	}
}

func TestTurnsCommandKeepsProjectionOnRuntimeFailure(t *testing.T) {
	model := readyModel(t)
	runner := &turnRecoveryTestRunner{
		turn: session.TurnRecord{ID: "turn-1", State: session.TurnInterrupted},
		err:  errors.New("turn journal unavailable"),
	}
	model.Model.Runner = runner
	model.Model.InterruptedTurns = []session.TurnRecord{runner.turn}

	model, cmd := model.handleCommand("/turns abort turn-1")
	model, _ = model.update(cmd())
	if model.Model.InterruptedTurnRequest != 0 || len(model.Model.InterruptedTurns) != 1 {
		t.Fatalf(
			"failed discard mutated projection = request %d turns %#v",
			model.Model.InterruptedTurnRequest,
			model.Model.InterruptedTurns,
		)
	}
}

func TestStaleInterruptedTurnDiscardCannotMutateNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.Model.InterruptedTurnRequest = 1
	model.Model.InterruptedTurns = []session.TurnRecord{{ID: "new-turn", State: session.TurnInterrupted}}

	next, cmd := model.handleInterruptedTurnAborted(interruptedTurnAbortedMsg{
		generation: 1,
		requestID:  1,
		turn:       session.TurnRecord{ID: "new-turn", State: session.TurnAborted},
	})
	if cmd != nil {
		t.Fatal("stale discard returned a command")
	}
	if next.Model.InterruptedTurnRequest != 1 || len(next.Model.InterruptedTurns) != 1 ||
		next.Model.InterruptedTurns[0].State != session.TurnInterrupted {
		t.Fatalf(
			"stale discard mutated projection: request=%d turns=%#v",
			next.Model.InterruptedTurnRequest,
			next.Model.InterruptedTurns,
		)
	}
}
