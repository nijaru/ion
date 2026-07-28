package app

import (
	"context"
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestBranchSummaryPromptPassesCustomInstructionsToRunner(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{}
	model.Model.Runner = runner
	model.Picker.Tree = &treePickerState{
		entries: []treePickerEntry{
			{id: "current", isLeaf: true},
			{id: "target", title: "target"},
		},
	}

	var cmd tea.Cmd
	model, _ = model.handleTreePickerKey(tea.KeyPressMsg{Code: tea.KeyDown})
	model, _ = model.handleTreePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	if model.Picker.BranchSummary == nil {
		t.Fatal("branch summary prompt did not open")
	}
	model, _ = model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyDown})
	model, _ = model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyDown})
	model, _ = model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	if !model.Picker.BranchSummary.custom {
		t.Fatal("custom instruction editor did not open")
	}
	model, _ = model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Text: "focus on errors"})
	model, cmd = model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	if !model.Picker.BranchSummary.navigating {
		t.Fatal("branch navigation did not start")
	}
	if cmd == nil {
		t.Fatal("branch navigation command is nil")
	}
	_ = cmd()
	if runner.navigates != 1 || runner.navigateID != "target" {
		t.Fatalf("runner navigation = %d calls to %q, want one call to target", runner.navigates, runner.navigateID)
	}
	if !runner.navigateOpts.Summarize || runner.navigateOpts.CustomInstructions != "focus on errors" {
		t.Fatalf("runner options = %#v, want summarize with custom instructions", runner.navigateOpts)
	}
}

func TestBranchSummaryNavigationWaitsForSettlement(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{}
	model.InFlight.AwaitingSettlement = true
	model.Picker.BranchSummary = &branchSummaryPromptState{targetID: "target"}

	next, cmd := model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	if cmd != nil {
		t.Fatal("branch navigation started before settlement")
	}
	if next.Picker.BranchSummary == nil || next.Picker.BranchSummary.navigating {
		t.Fatalf("branch prompt = %#v, want navigation blocked", next.Picker.BranchSummary)
	}
	if !strings.Contains(next.Picker.BranchSummary.err, "Finish or cancel the current turn") {
		t.Fatalf("branch prompt error = %q, want settlement barrier", next.Picker.BranchSummary.err)
	}
}

func TestBranchSummaryPromptCancelReturnsToTree(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{}
	model.Picker.Tree = &treePickerState{entries: []treePickerEntry{{id: "current", isLeaf: true}, {id: "target"}}}
	model, _ = model.handleTreePickerKey(tea.KeyPressMsg{Code: tea.KeyDown})
	model, _ = model.handleTreePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model, _ = model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if model.Picker.BranchSummary != nil || model.Picker.Tree == nil {
		t.Fatalf(
			"after cancel branch prompt=%v tree=%v, want prompt closed and tree open",
			model.Picker.BranchSummary,
			model.Picker.Tree,
		)
	}
}

func TestBranchSummaryWithoutRunnerReturnsTerminalCommand(t *testing.T) {
	model := readyModel(t)

	_, cmd := model.openBranchSummaryPrompt("target")
	requireTerminalCommitContains(t, cmd, "tree navigation is unavailable")
}

func TestTreeNavigationCompletionCancelsRequestContext(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 1
	ctx, cancel := context.WithCancel(context.Background())
	model.Model.treeNavigationCancel = cancel

	next, _ := model.handleTreePickerMove(treePickerMoveMsg{generation: 1, requestID: 1})
	select {
	case <-ctx.Done():
	default:
		t.Fatal("navigation completion did not cancel its child context")
	}
	if next.Model.treeNavigationCancel != nil {
		t.Fatal("completed navigation retained its cancellation function")
	}
}

func TestBranchSummaryNavigationResultClosesPromptAndTree(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{}
	model.Picker.Tree = &treePickerState{entries: []treePickerEntry{{id: "current", isLeaf: true}, {id: "target"}}}
	model.Picker.BranchSummary = &branchSummaryPromptState{targetID: "target", navigating: true}
	model, _ = model.handleTreePickerMove(treePickerMoveMsg{})
	if model.Picker.Tree != nil || model.Picker.BranchSummary != nil {
		t.Fatalf(
			"after successful navigation tree=%v prompt=%v, want both closed",
			model.Picker.Tree,
			model.Picker.BranchSummary,
		)
	}
}

func TestBranchSummaryNavigationCancelUsesRequestContext(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{}
	model.Model.TreeNavigationRequest = 1
	model.Picker.Tree = &treePickerState{}
	model.Picker.BranchSummary = &branchSummaryPromptState{
		targetID:   "target",
		navigating: true,
	}
	ctx, cancel := context.WithCancel(context.Background())
	model.Model.treeNavigationCancel = cancel

	model, cmd := model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if cmd == nil {
		t.Fatal("cancel did not return a request cancellation command")
	}
	if model.Picker.BranchSummary == nil || !model.Picker.BranchSummary.navigating {
		t.Fatal("navigation should remain pending until its cancellation result arrives")
	}

	msg, ok := cmd().(branchNavigationCancelMsg)
	if !ok || msg.err != nil || msg.requestID != 1 {
		t.Fatalf("cancel message = %#v, want successful request cancellation", msg)
	}
	select {
	case <-ctx.Done():
	default:
		t.Fatal("navigation request context was not canceled")
	}

	next, _ := model.handleBranchNavigationCancel(msg)
	if next.Picker.BranchSummary == nil || next.Picker.BranchSummary.navigating {
		t.Fatalf("after cancellation result prompt = %#v, want retryable prompt", next.Picker.BranchSummary)
	}
}

func TestStaleNavigationCancelCannotCancelNewRequest(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{}
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 1
	model.Picker.BranchSummary = &branchSummaryPromptState{targetID: "target", navigating: true}
	ctxA, cancelA := context.WithCancel(context.Background())
	model.Model.treeNavigationCancel = cancelA

	_, cancelCmd := model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if cancelCmd == nil {
		t.Fatal("stale navigation did not create cancellation command")
	}
	ctxB, cancelB := context.WithCancel(context.Background())
	model.Model.TreeNavigationRequest = 2
	model.Model.treeNavigationCancel = cancelB
	cancelMsg := cancelCmd().(branchNavigationCancelMsg)
	select {
	case <-ctxA.Done():
	default:
		t.Fatal("old navigation context was not canceled")
	}
	select {
	case <-ctxB.Done():
		t.Fatal("stale cancellation canceled the newer navigation context")
	default:
	}

	next, cmd := model.handleBranchNavigationCancel(cancelMsg)
	if cmd != nil || next.Model.TreeNavigationRequest != 2 || next.Picker.BranchSummary == nil {
		t.Fatalf("stale cancellation result mutated newer request: model=%#v cmd=%v", next.Model, cmd != nil)
	}
}

func TestStaleBranchNavigationCancelCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.Progress.Status = "new runtime"
	model.Picker.BranchSummary = &branchSummaryPromptState{navigating: true}

	next, cmd := model.handleBranchNavigationCancel(branchNavigationCancelMsg{
		generation: 1,
		err:        errors.New("old runtime abort failed"),
	})
	if cmd != nil {
		t.Fatal("stale branch navigation cancellation returned a command")
	}
	if next.Progress.Status != "new runtime" || next.Picker.BranchSummary == nil {
		t.Fatalf(
			"stale branch navigation cancellation mutated new runtime: status=%q prompt=%#v",
			next.Progress.Status,
			next.Picker.BranchSummary,
		)
	}
}
