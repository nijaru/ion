package app

import (
	"context"
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
)

type approvalTestRunner struct {
	stubRunner
	id       string
	decision session.ApprovalDecision
	calls    int
}

func (r *approvalTestRunner) ResolveApproval(id string, decision session.ApprovalDecision) error {
	if id != r.id {
		return context.Canceled
	}
	r.calls++
	r.decision = decision
	return nil
}

func TestApprovalPromptRendersAndResolvesAlways(t *testing.T) {
	model := readyModel(t)
	runner := &approvalTestRunner{id: "approval-1"}
	model.Model.Runner = runner

	updated, cmd := model.handleSessionEvent(session.ApprovalRequest{
		ID:         "approval-1",
		ToolCallID: "call-1",
		ToolName:   "write",
		Category:   "write",
		Operation:  "write",
		Resource:   "config.toml",
	})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("approval request did not continue event consumption")
	}
	if model.Picker.Approval == nil {
		t.Fatal("approval prompt not opened")
	}
	if rendered := model.renderApprovalPrompt(); !strings.Contains(rendered, "config.toml") {
		t.Fatalf("approval prompt = %q, want resource", rendered)
	}

	updated, cmd = model.handleKey(tea.KeyPressMsg{Text: "a"})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("approval key did not return resolver command")
	}
	result, ok := cmd().(approvalResolveMsg)
	if !ok || result.generation != model.Model.EventGeneration || result.err != nil {
		t.Fatalf("approval command result = %#v", result)
	}
	if runner.calls != 1 || runner.decision != session.ApprovalAlways {
		t.Fatalf("resolver calls/decision = %d/%q, want 1/always", runner.calls, runner.decision)
	}
	if model.Picker.Approval == nil || !model.Picker.Approval.resolving {
		t.Fatal("approval prompt should remain resolving until resolution event")
	}

	model, _ = model.handleSessionEvent(session.ApprovalResolution{
		ID:       "approval-1",
		Decision: session.ApprovalAlways,
	})
	if model.Picker.Approval != nil {
		t.Fatal("approval prompt remained after resolution")
	}
}

func TestStaleApprovalResolutionCannotCancelNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.Progress.Status = "new runtime"
	model.Picker.Approval = &approvalPromptState{
		request:   session.ApprovalRequest{ID: "new-approval"},
		resolving: true,
	}

	next, cmd := model.handleApprovalResolve(approvalResolveMsg{
		generation: 1,
		err:        errors.New("old approval failed"),
	})
	if cmd != nil {
		t.Fatal("stale approval result returned a command")
	}
	if next.Picker.Approval == nil || next.Progress.Status != "new runtime" {
		t.Fatalf(
			"stale approval result mutated new runtime: picker=%#v status=%q",
			next.Picker.Approval,
			next.Progress.Status,
		)
	}
}

func TestApprovalPromptEscDenies(t *testing.T) {
	model := readyModel(t)
	runner := &approvalTestRunner{id: "approval-2"}
	model.Model.Runner = runner
	model.Picker.Approval = &approvalPromptState{request: session.ApprovalRequest{ID: "approval-2"}}

	updated, cmd := model.handleKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("escape did not return resolver command")
	}
	if result := cmd().(approvalResolveMsg); result.err != nil {
		t.Fatalf("deny command: %v", result.err)
	}
	if runner.decision != session.ApprovalDeny {
		t.Fatalf("decision = %q, want deny", runner.decision)
	}
}
