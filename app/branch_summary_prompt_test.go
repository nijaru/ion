package app

import (
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
		t.Fatalf("after cancel branch prompt=%v tree=%v, want prompt closed and tree open", model.Picker.BranchSummary, model.Picker.Tree)
	}
}

func TestBranchSummaryNavigationResultClosesPromptAndTree(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{}
	model.Picker.Tree = &treePickerState{entries: []treePickerEntry{{id: "current", isLeaf: true}, {id: "target"}}}
	model.Picker.BranchSummary = &branchSummaryPromptState{targetID: "target", navigating: true}
	model, _ = model.handleTreePickerMove(treePickerMoveMsg{})
	if model.Picker.Tree != nil || model.Picker.BranchSummary != nil {
		t.Fatalf("after successful navigation tree=%v prompt=%v, want both closed", model.Picker.Tree, model.Picker.BranchSummary)
	}
}
