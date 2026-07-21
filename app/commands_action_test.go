package app

import (
	"context"
	"errors"
	"testing"

	"github.com/nijaru/ion/session"
)

type actionRecoveryTestRunner struct {
	stubRunner
	action session.ActionRecord
	err    error
	got    struct {
		id           string
		state        session.ActionState
		verification string
	}
}

func (r *actionRecoveryTestRunner) UnsettledActions(context.Context) ([]session.ActionRecord, error) {
	return []session.ActionRecord{r.action}, nil
}

func (r *actionRecoveryTestRunner) ReconcileAction(_ context.Context, id string, state session.ActionState, verification, _, _, _ string) (session.ActionRecord, error) {
	r.got.id = id
	r.got.state = state
	r.got.verification = verification
	if r.err != nil {
		return session.ActionRecord{}, r.err
	}
	r.action.State = state
	return r.action, nil
}

func TestActionsCommandReconcilesWithExplicitEvidence(t *testing.T) {
	model := readyModel(t)
	runner := &actionRecoveryTestRunner{action: session.ActionRecord{
		ID: "action-1", Tool: "bash", State: session.ActionIndeterminate,
	}}
	model.Model.Runner = runner
	model.Model.Recovery = []session.ActionRecord{runner.action}

	model, cmd := model.handleCommand("/actions reconcile action-1 completed")
	if cmd == nil {
		t.Fatal("missing evidence unexpectedly accepted")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "reconciliation evidence is required" {
		t.Fatalf("missing evidence error = %v", err)
	}
	if model.Model.RecoveryRequest != 0 {
		t.Fatal("rejected reconciliation left a request in flight")
	}

	model, cmd = model.handleCommand("/actions reconcile action-1 completed operator verified result")
	if cmd == nil || model.Model.RecoveryRequest != 1 {
		t.Fatalf("accepted reconciliation = request %d, cmd %v; want request 1", model.Model.RecoveryRequest, cmd != nil)
	}
	msg := cmd()
	if got := msg.(actionReconciledMsg); got.err != nil {
		t.Fatalf("reconcile result error = %v", got.err)
	}
	model, _ = model.update(msg)
	if model.Model.RecoveryRequest != 0 || len(model.Model.Recovery) != 0 {
		t.Fatalf("post-reconcile projection = request %d recovery %#v", model.Model.RecoveryRequest, model.Model.Recovery)
	}
	if runner.got.id != "action-1" || runner.got.state != session.ActionCompleted || runner.got.verification != "operator verified result" {
		t.Fatalf("reconcile request = %#v, want exact action and evidence", runner.got)
	}
}

func TestActionsCommandRejectsUnknownActionAndRuntimeFailure(t *testing.T) {
	model := readyModel(t)
	runner := &actionRecoveryTestRunner{
		action: session.ActionRecord{ID: "action-1", State: session.ActionIndeterminate},
		err:    errors.New("journal unavailable"),
	}
	model.Model.Runner = runner
	model.Model.Recovery = []session.ActionRecord{runner.action}

	_, cmd := model.handleCommand("/actions reconcile missing completed verified")
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != `action "missing" is not an unsettled action; run /actions` {
		t.Fatalf("unknown action error = %v", err)
	}

	model, cmd = model.handleCommand("/actions reconcile action-1 failed provider reported failure")
	msg := cmd()
	model, cmd = model.update(msg)
	if cmd == nil {
		t.Fatal("runtime failure did not produce a UI error command")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "reconcile action: journal unavailable" {
		t.Fatalf("runtime failure error = %v", err)
	}
	if model.Model.RecoveryRequest != 0 || len(model.Model.Recovery) != 1 {
		t.Fatalf("failed reconciliation mutated projection: request %d recovery %#v", model.Model.RecoveryRequest, model.Model.Recovery)
	}
}
