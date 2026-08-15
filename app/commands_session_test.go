package app

import (
	"context"
	"errors"
	"fmt"
	"os"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/session"
)

type sessionNamingTestRunner struct {
	stubRunner
	ctx               context.Context
	nameExpectedLeaf  string
	nameEntryID       string
	name              string
	err               error
	labelCtx          context.Context
	labelExpectedLeaf string
	labelLeaf         string
	labelEntryID      string
	labelValue        string
	labelErr          error
	exportCtx         context.Context
	exportID          string
	exportErr         error
}

func (r *sessionNamingTestRunner) AppendSessionInfo(ctx context.Context, expectedLeafID, name string) (string, error) {
	r.ctx = ctx
	r.nameExpectedLeaf = expectedLeafID
	r.name = name
	return r.nameEntryID, r.err
}

func (r *sessionNamingTestRunner) GetLabel(ctx context.Context, leafID string) (string, error) {
	r.labelCtx = ctx
	r.labelLeaf = leafID
	return r.labelValue, r.labelErr
}

func (r *sessionNamingTestRunner) GetBranchLabel(ctx context.Context, leafID string) (string, error) {
	r.labelCtx = ctx
	r.labelLeaf = leafID
	return r.labelValue, r.labelErr
}

func (r *sessionNamingTestRunner) AppendLabel(
	ctx context.Context,
	expectedLeafID, leafID, label string,
) (string, error) {
	r.labelCtx = ctx
	r.labelExpectedLeaf = expectedLeafID
	r.labelLeaf = leafID
	r.labelValue = label
	return r.labelEntryID, r.labelErr
}

func (r *sessionNamingTestRunner) ExportSessionBundle(
	ctx context.Context,
	sessionID string,
) (ionexport.SessionBundle, error) {
	r.exportCtx = ctx
	r.exportID = sessionID
	return ionexport.SessionBundle{}, r.exportErr
}

func TestSessionNameUsesRuntimeOperationContext(t *testing.T) {
	model := readyModel(t)
	model.Model.LeafID = "leaf-1"
	expectedContext := model.Model.runtimeContext
	runner := &sessionNamingTestRunner{nameEntryID: "name-entry"}
	model.Model.Runner = runner

	updated, cmd := model.nameSession("daily driver")
	if cmd == nil {
		t.Fatal("session name did not return a command")
	}
	result, ok := cmd().(sessionNamedMsg)
	if !ok || result.generation != model.Model.EventGeneration || result.err != nil {
		t.Fatalf("session name result = %#v", result)
	}
	if runner.ctx != expectedContext ||
		runner.nameExpectedLeaf != "leaf-1" ||
		runner.name != "daily driver" {
		t.Fatalf(
			"session name request = context %v, expected leaf %q, name %q",
			runner.ctx,
			runner.nameExpectedLeaf,
			runner.name,
		)
	}
	next, cmd := updated.handleSessionNamed(result)
	requireTerminalCommitContains(t, cmd, "Session named: daily driver")
	if next.Model.LeafID != "name-entry" {
		t.Fatalf("session name leaf = %q, want name-entry", next.Model.LeafID)
	}
}

func TestStaleSessionNameCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd := model.handleSessionNamed(sessionNamedMsg{
		generation: 1,
		name:       "old runtime",
		err:        errors.New("old runtime name failed"),
	})
	if cmd != nil {
		t.Fatal("stale session name returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale session name rendered into the new runtime")
	}
}

func TestSessionLabelUsesRuntimeOperationContext(t *testing.T) {
	model := readyModel(t)
	model.Model.LeafID = "leaf-1"
	expectedContext := model.Model.runtimeContext
	runner := &sessionNamingTestRunner{labelEntryID: "label-entry"}
	model.Model.Runner = runner

	updated, cmd := model.handleLabelCommand([]string{"/label", "release candidate"})
	if cmd == nil {
		t.Fatal("session label did not return a command")
	}
	result, ok := cmd().(labelShowMsg)
	if !ok || result.generation != model.Model.EventGeneration || result.err != nil {
		t.Fatalf("session label result = %#v", result)
	}
	if runner.labelCtx != expectedContext ||
		runner.labelExpectedLeaf != "leaf-1" ||
		runner.labelLeaf != "leaf-1" ||
		runner.labelValue != "release candidate" {
		t.Fatalf(
			"session label request = context %v, expected leaf %q, leaf %q, label %q",
			runner.labelCtx,
			runner.labelExpectedLeaf,
			runner.labelLeaf,
			runner.labelValue,
		)
	}
	next, cmd := updated.handleLabelShow(result)
	requireTerminalCommitContains(t, cmd, "🏷 label: release candidate")
	if next.Model.LeafID != "label-entry" {
		t.Fatalf("session label leaf = %q, want label-entry", next.Model.LeafID)
	}

	_, cmd = updated.handleLabelShow(labelShowMsg{
		generation: model.Model.EventGeneration,
		err:        errors.New("label lookup failed"),
	})
	requireTerminalCommitContains(t, cmd, "⚠ label: label lookup failed")
}

func TestStaleSessionLabelCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd := model.handleLabelShow(labelShowMsg{
		generation: 1,
		label:      "old runtime",
		err:        errors.New("old runtime label failed"),
	})
	if cmd != nil {
		t.Fatal("stale session label returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale session label rendered into the new runtime")
	}
}

func TestStaleSessionMetadataCannotRenderNavigatedBranch(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.App.PrintedTranscript = false

	next, cmd := model.handleSessionNamed(sessionNamedMsg{
		generation:            1,
		treeNavigationRequest: 1,
		name:                  "old branch",
		err:                   errors.New("old name failed"),
	})
	if cmd != nil {
		t.Fatal("stale same-runtime session name returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale same-runtime session name rendered into the navigated branch")
	}

	next, cmd = model.handleLabelShow(labelShowMsg{
		generation:            1,
		treeNavigationRequest: 1,
		label:                 "old branch",
		err:                   errors.New("old label failed"),
	})
	if cmd != nil {
		t.Fatal("stale same-runtime label returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale same-runtime label rendered into the navigated branch")
	}
}

func TestDelayedSessionMetadataCannotRegressCurrentLeaf(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.LeafID = "current-leaf"
	model.App.PrintedTranscript = false

	next, cmd := model.handleSessionNamed(sessionNamedMsg{
		generation:     1,
		expectedLeafID: "old-leaf",
		leafID:         "old-name-entry",
		name:           "old name",
	})
	if cmd != nil {
		t.Fatal("delayed session name returned a command")
	}
	if next.Model.LeafID != "current-leaf" || next.App.PrintedTranscript {
		t.Fatalf(
			"delayed session name changed projection: leaf=%q printed=%v",
			next.Model.LeafID,
			next.App.PrintedTranscript,
		)
	}

	next, cmd = model.handleLabelShow(labelShowMsg{
		generation:     1,
		expectedLeafID: "old-leaf",
		leafID:         "old-label-entry",
		label:          "old label",
	})
	if cmd != nil {
		t.Fatal("delayed label returned a command")
	}
	if next.Model.LeafID != "current-leaf" || next.App.PrintedTranscript {
		t.Fatalf("delayed label changed projection: leaf=%q printed=%v", next.Model.LeafID, next.App.PrintedTranscript)
	}
}

func TestMetadataCompletionSurvivesRejectedNavigationEpoch(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Model.LeafID = "current-leaf"

	next, cmd := model.handleSessionNamed(sessionNamedMsg{
		generation:            1,
		treeNavigationRequest: 1,
		expectedLeafID:        "current-leaf",
		leafID:                "named-leaf",
		name:                  "current name",
	})
	requireTerminalCommitContains(t, cmd, "Session named: current name")
	if next.Model.LeafID != "named-leaf" {
		t.Fatalf("session name leaf = %q, want named-leaf", next.Model.LeafID)
	}

	next.Model.LeafID = "current-leaf"
	next.App.PrintedTranscript = false
	next, cmd = next.handleLabelShow(labelShowMsg{
		generation:            1,
		treeNavigationRequest: 1,
		expectedLeafID:        "current-leaf",
		leafID:                "labeled-leaf",
		label:                 "current label",
	})
	requireTerminalCommitContains(t, cmd, "🏷 label: current label")
	if next.Model.LeafID != "labeled-leaf" {
		t.Fatalf("session label leaf = %q, want labeled-leaf", next.Model.LeafID)
	}
}

func TestSessionExportUsesRuntimeOperationContext(t *testing.T) {
	t.Chdir(t.TempDir())
	model := readyModel(t)
	model.Model.LeafID = "session-1"
	expectedContext := model.Model.runtimeContext
	runner := &sessionNamingTestRunner{}
	model.Model.Runner = runner

	updated, cmd := model.exportSession()
	if cmd == nil {
		t.Fatal("session export did not return a command")
	}
	result, ok := cmd().(sessionExportedMsg)
	if !ok || result.generation != model.Model.EventGeneration || result.err != nil {
		t.Fatalf("session export result = %#v", result)
	}
	if runner.exportCtx != expectedContext || runner.exportID != "session-1" {
		t.Fatalf("session export request = context %v, ID %q", runner.exportCtx, runner.exportID)
	}
	if _, err := os.Stat(result.filename); err != nil {
		t.Fatalf("export file %q: %v", result.filename, err)
	}
	_, cmd = updated.handleSessionExported(result)
	requireTerminalCommitContains(t, cmd, "Exported session to "+result.filename)
}

func TestSuccessfulClipboardResultReturnsTerminalCommand(t *testing.T) {
	model := readyModel(t)

	_, cmd := model.handleSessionCopied(sessionCopiedMsg{generation: model.Model.EventGeneration})
	requireTerminalCommitContains(t, cmd, "Copied last response to clipboard")
}

func TestStaleSessionExportCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd := model.handleSessionExported(sessionExportedMsg{
		generation: 1,
		filename:   "old-runtime.json",
		err:        errors.New("old runtime export failed"),
	})
	if cmd != nil {
		t.Fatal("stale session export returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale session export rendered into the new runtime")
	}
}

func TestCompactCommandUsesRunner(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{}
	model.Model.Runner = runner

	updated, cmd := model.handleCompactCommand()
	if cmd == nil {
		t.Fatal("compact command is nil")
	}
	if !updated.Progress.Compacting {
		t.Fatal("compaction state is not active")
	}
	msg := cmd()
	result, ok := msg.(sessionCompactedMsg)
	if !ok {
		t.Fatalf("compact result = %T, want sessionCompactedMsg", msg)
	}
	if result.notice == "" {
		t.Fatal("compact result has no notice")
	}
	if result.generation != model.Model.EventGeneration {
		t.Fatalf("compact result generation = %d, want %d", result.generation, model.Model.EventGeneration)
	}
	if runner.compacts != 1 {
		t.Fatalf("runner compact calls = %d, want 1", runner.compacts)
	}

	// Keep the command result compatible with Bubble Tea's message contract.
	var _ tea.Msg = result
}

func TestStaleCompactionResultCannotReleaseNewRuntimeQueue(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.Progress.Compacting = true
	model.InFlight.QueuedTurns = []string{"queued for current runtime"}

	next, cmd, handled := model.dispatchAppControlMessage(sessionCompactedMsg{
		generation: 1,
		notice:     "old runtime compacted",
	})
	if !handled {
		t.Fatal("stale compaction result was not handled")
	}
	if cmd != nil {
		t.Fatal("stale compaction result returned a command")
	}
	if !next.Progress.Compacting {
		t.Fatal("stale compaction result cleared current compaction state")
	}
	if got := len(next.InFlight.QueuedTurns); got != 1 {
		t.Fatalf("queued turns = %d, want one current-runtime queue entry", got)
	}
}

func TestEscapeCancelsCompaction(t *testing.T) {
	model := readyModel(t)
	called := false
	model.Progress.Compacting = true
	model.Model.compactionCancel = func() { called = true }

	next, cmd := model.handleKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if !called {
		t.Fatal("escape did not cancel active compaction")
	}
	if cmd == nil {
		t.Fatal("escape returned no cancellation notice command")
	}
	if !next.Progress.Compacting {
		t.Fatal("escape cleared compacting before runtime settled")
	}
}

func TestCompactionFailureClearsBusyState(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{compactErr: fmt.Errorf("context budget unavailable")}
	model.Model.Runner = runner

	updated, cmd := model.handleCompactCommand()
	if cmd == nil {
		t.Fatal("compact command is nil")
	}
	msg := cmd()
	result, ok := msg.(sessionCompactedMsg)
	if !ok {
		t.Fatalf("compact result = %T, want sessionCompactedMsg", msg)
	}
	if result.err == nil {
		t.Fatal("compact error = nil, want compaction failure")
	}

	next, followUp := updated.handleSessionCompacted(result)
	if followUp == nil {
		t.Fatal("compaction failure did not return an error presentation command")
	}
	if next.Progress.Compacting {
		t.Fatal("compaction failure left compacting state active")
	}
}

func TestUndoCommand(t *testing.T) {
	model := readyModel(t)
	now := time.Now()
	userMsg := session.NewUserText("write a web server in go", now)
	userEntry := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "entry-user", ParentID: "entry-root", Timestamp: now},
		Message:   userMsg,
	}
	assistantMsg := &session.AssistantMessage{
		Content: []session.Content{session.TextContent{Text: "here is the server"}},
	}
	assistantEntry := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "entry-assistant", ParentID: "entry-user", Timestamp: now},
		Message:   assistantMsg,
	}

	runner := &stubRunner{
		branchEntries: []session.Entry{userEntry, assistantEntry},
	}
	model.Model.Runner = runner
	model.Model.LeafID = "entry-assistant"

	updated, cmd := model.handleUndoCommand()
	if cmd == nil {
		t.Fatal("undo command returned nil cmd")
	}
	msg := cmd()
	undoMsg, ok := msg.(undoResultMsg)
	if !ok {
		t.Fatalf("undo result msg = %T, want undoResultMsg", msg)
	}
	if undoMsg.err != nil {
		t.Fatalf("undo error: %v", undoMsg.err)
	}
	if undoMsg.targetLeafID != "entry-root" {
		t.Fatalf("undo targetLeafID = %q, want entry-root", undoMsg.targetLeafID)
	}
	if undoMsg.promptText != "write a web server in go" {
		t.Fatalf("undo promptText = %q, want 'write a web server in go'", undoMsg.promptText)
	}

	next, followUp := updated.handleUndoResult(undoMsg)
	if followUp == nil {
		t.Fatal("handleUndoResult returned nil followUp cmd")
	}
	if next.Model.LeafID != "entry-root" {
		t.Fatalf("model leaf after undo = %q, want entry-root", next.Model.LeafID)
	}
	if got := next.Input.Composer.Value(); got != "write a web server in go" {
		t.Fatalf("composer value after undo = %q, want 'write a web server in go'", got)
	}
}

func TestDiffCommand(t *testing.T) {
	model := readyModel(t)
	model.App.Workdir = t.TempDir()

	updated, cmd := model.handleDiffCommand([]string{"/diff"})
	if cmd == nil {
		t.Fatal("diff command returned nil cmd")
	}
	msg := cmd()
	diffMsg, ok := msg.(diffResultMsg)
	if !ok {
		t.Fatalf("diff result msg = %T, want diffResultMsg", msg)
	}
	// In an empty temp dir (not a git repo), diffMsg returns error, handled gracefully
	next, followUp := updated.handleDiffResult(diffMsg)
	if followUp == nil {
		t.Fatal("handleDiffResult returned nil followUp cmd")
	}
	_ = next
}

func TestShareCommand(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.LeafID = "leaf-1"

	runner := &sessionNamingTestRunner{}
	model.Model.Runner = runner

	updated, cmd := model.handleShareCommand()
	if cmd == nil {
		t.Fatal("share command returned nil cmd")
	}

	// Dispatch shared msg
	sharedMsg := sessionSharedMsg{
		generation: 1,
		gistURL:    "https://gist.github.com/secret123",
	}
	next, followUp, handled := updated.dispatchPickerControllerMessage(sharedMsg)
	if !handled {
		t.Fatal("sessionSharedMsg not handled")
	}
	if followUp == nil {
		t.Fatal("handleSessionShared returned nil followUp cmd")
	}
	_ = next
}
