package app

import (
	"context"
	"errors"
	"fmt"
	"os"
	"testing"

	tea "charm.land/bubbletea/v2"
	ionexport "github.com/nijaru/ion/internal/export"
)

type sessionNamingTestRunner struct {
	stubRunner
	ctx        context.Context
	name       string
	err        error
	labelCtx   context.Context
	labelLeaf  string
	labelValue string
	labelErr   error
	exportCtx  context.Context
	exportID   string
	exportErr  error
}

func (r *sessionNamingTestRunner) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	r.ctx = ctx
	r.name = name
	return "", r.err
}

func (r *sessionNamingTestRunner) GetLabel(ctx context.Context, leafID string) (string, error) {
	r.labelCtx = ctx
	r.labelLeaf = leafID
	return r.labelValue, r.labelErr
}

func (r *sessionNamingTestRunner) AppendLabel(ctx context.Context, leafID, label string) (string, error) {
	r.labelCtx = ctx
	r.labelLeaf = leafID
	r.labelValue = label
	return "", r.labelErr
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
	expectedContext := model.Model.runtimeContext
	runner := &sessionNamingTestRunner{}
	model.Model.Runner = runner

	updated, cmd := model.nameSession("daily driver")
	if cmd == nil {
		t.Fatal("session name did not return a command")
	}
	result, ok := cmd().(sessionNamedMsg)
	if !ok || result.generation != model.Model.EventGeneration || result.err != nil {
		t.Fatalf("session name result = %#v", result)
	}
	if runner.ctx != expectedContext || runner.name != "daily driver" {
		t.Fatalf("session name request = context %v, name %q", runner.ctx, runner.name)
	}
	if _, cmd := updated.handleSessionNamed(result); cmd != nil {
		t.Fatal("current session name returned an unexpected command")
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
	runner := &sessionNamingTestRunner{}
	model.Model.Runner = runner

	updated, cmd := model.handleLabelCommand([]string{"/label", "release candidate"})
	if cmd == nil {
		t.Fatal("session label did not return a command")
	}
	result, ok := cmd().(labelShowMsg)
	if !ok || result.generation != model.Model.EventGeneration || result.err != nil {
		t.Fatalf("session label result = %#v", result)
	}
	if runner.labelCtx != expectedContext || runner.labelLeaf != "leaf-1" || runner.labelValue != "release candidate" {
		t.Fatalf(
			"session label request = context %v, leaf %q, label %q",
			runner.labelCtx,
			runner.labelLeaf,
			runner.labelValue,
		)
	}
	if _, cmd := updated.handleLabelShow(result); cmd != nil {
		t.Fatal("current session label returned an unexpected command")
	}
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
	if _, cmd := updated.handleSessionExported(result); cmd != nil {
		t.Fatal("current session export returned an unexpected command")
	}
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
	model.turnReducer().QueueTurn("queued for current runtime")

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
