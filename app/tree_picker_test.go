package app

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

func TestLoadSessionTreeProjectsPersistedLineageAndChildren(t *testing.T) {
	store, err := session.NewSQLiteStore(":memory:", "tree-test")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	sess := session.NewSession(store, 16)
	ctx := context.Background()

	rootID, err := sess.AppendMessage(ctx, session.NewUserText("root", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	childID, err := sess.AppendMessage(ctx, session.NewUserText("child", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetLeafID(rootID); err != nil {
		t.Fatal(err)
	}

	entries, err := store.Entries(ctx)
	if err != nil {
		t.Fatal(err)
	}
	tree, err := loadSessionTree(agent.SessionTreeSnapshot{
		LeafID:  rootID,
		Entries: entries,
	})
	if err != nil {
		t.Fatal(err)
	}
	if tree.Current == nil || tree.Current.ID() != rootID {
		t.Fatalf("current = %#v, want root %q", tree.Current, rootID)
	}
	if len(tree.Lineage) != 0 {
		t.Fatalf("lineage = %d entries, want empty root lineage", len(tree.Lineage))
	}
	if len(tree.Children) != 1 || tree.Children[0].ID() != childID {
		t.Fatalf("children = %#v, want child %q", tree.Children, childID)
	}
}

func TestStaleTreePickerResultsCannotMutateNewRuntimeGeneration(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.Picker.Tree = &treePickerState{
		entries: []treePickerEntry{{id: "current", isLeaf: true}},
	}
	model.Picker.BranchSummary = &branchSummaryPromptState{targetID: "current"}

	staleTree := treePickerLoadedMsg{
		generation: 1,
		tree:       SessionTree{Current: agentMsgEntry("stale")},
	}
	next, cmd, handled := model.dispatchPickerControllerMessage(staleTree)
	if !handled || cmd != nil || len(next.Picker.Tree.entries) != 1 || next.Picker.Tree.entries[0].id != "current" {
		t.Fatalf(
			"stale tree result handling = (handled=%v, cmd=%v, tree=%#v), want ignored",
			handled,
			cmd != nil,
			next.Picker.Tree,
		)
	}

	next, cmd, handled = model.dispatchPickerControllerMessage(
		treePickerMoveMsg{generation: 1, err: errors.New("stale")},
	)
	if !handled || cmd != nil || next.Picker.BranchSummary == nil {
		t.Fatalf(
			"stale tree move handling = (handled=%v, cmd=%v, prompt=%#v), want ignored",
			handled,
			cmd != nil,
			next.Picker.BranchSummary,
		)
	}

	next, cmd, handled = model.dispatchPickerControllerMessage(
		replayBranchMsg{generation: 1, entries: []session.Entry{agentMsgEntry("stale")}},
	)
	if !handled || cmd != nil || next.Progress.LastError != "" {
		t.Fatalf(
			"stale replay handling = (handled=%v, cmd=%v, error=%q), want ignored",
			handled,
			cmd != nil,
			next.Progress.LastError,
		)
	}
}

func TestStaleSameGenerationBranchReplayCannotOverwriteCurrentNavigation(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Progress.LastError = "keep current projection"

	next, cmd, handled := model.dispatchPickerControllerMessage(replayBranchMsg{
		generation: 1,
		requestID:  1,
		entries:    []session.Entry{agentMsgEntry("stale")},
	})
	if !handled || cmd != nil || next.Progress.LastError != "keep current projection" {
		t.Fatalf(
			"stale same-generation replay = (handled=%v, cmd=%v, error=%q), want ignored",
			handled,
			cmd != nil,
			next.Progress.LastError,
		)
	}
}

func TestSuccessfulTreeNavigationUpdatesSelectedLeafBeforeReplay(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 1
	model.Model.LeafID = "abandoned-leaf"
	model.Picker.BranchSummary = &branchSummaryPromptState{targetID: "selected-leaf"}

	next, _ := model.handleTreePickerMove(treePickerMoveMsg{
		generation: 1,
		requestID:  1,
		leafID:     "selected-leaf",
	})
	if got, want := next.Model.LeafID, "selected-leaf"; got != want {
		t.Fatalf("selected leaf = %q, want navigation leaf %q", got, want)
	}
}

func TestSuccessfulBranchReplayUpdatesSelectedLeaf(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 1
	model.Model.LeafID = "abandoned-leaf"
	entries := []session.Entry{agentMsgEntry("root"), agentMsgEntry("selected")}

	next, cmd, handled := model.dispatchPickerControllerMessage(replayBranchMsg{
		generation: 1,
		requestID:  1,
		entries:    entries,
	})
	if !handled {
		t.Fatal("branch replay was not handled")
	}
	if cmd == nil {
		t.Fatal("branch replay did not schedule terminal output")
	}
	if got, want := next.Model.LeafID, entries[len(entries)-1].ID(); got != want {
		t.Fatalf("selected leaf = %q, want replay leaf %q", got, want)
	}
}

func TestTreeNavigationFailuresReturnTerminalCommands(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1

	_, cmd := model.openTreePicker()
	requireTerminalCommitContains(t, cmd, "session tree is not available")

	_, cmd = model.handleTreePickerMove(treePickerMoveMsg{
		generation: 1,
		cancelled:  true,
	})
	requireTerminalCommitContains(t, cmd, "branch navigation cancelled")

	_, cmd = model.handleTreePickerMove(treePickerMoveMsg{
		generation: 1,
		err:        errors.New("navigation failed"),
	})
	requireTerminalCommitContains(t, cmd, "tree navigation failed: navigation failed")
}
