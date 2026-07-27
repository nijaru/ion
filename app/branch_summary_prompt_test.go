package app

import (
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

func TestBranchSummaryNavigationCancelSurfacesAbortError(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{abortErr: errors.New("runtime is closed")}
	model.Picker.Tree = &treePickerState{}
	model.Picker.BranchSummary = &branchSummaryPromptState{
		targetID:   "target",
		navigating: true,
	}

	model, cmd := model.handleBranchSummaryPromptKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if cmd == nil {
		t.Fatal("cancel did not return an abort command")
	}
	if model.Picker.BranchSummary == nil || !model.Picker.BranchSummary.navigating {
		t.Fatal("failed cancellation should keep navigation state until its result arrives")
	}

	msg := cmd()
	cancel, ok := msg.(branchNavigationCancelMsg)
	if !ok || cancel.err == nil {
		t.Fatalf("cancel message = %#v, want branch navigation cancellation", msg)
	}
	if !strings.Contains(cancel.err.Error(), "cancel branch navigation: runtime is closed") {
		t.Fatalf("cancel error = %v, want contextual runtime error", cancel.err)
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
