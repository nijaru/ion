package app

import (
	"errors"
	"strings"
	"testing"
)

func TestTerminalCommitMarksPrintedTranscript(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = false

	model.terminalCommit().MarkPrinted()

	if !model.App.PrintedTranscript {
		t.Fatal("printed transcript was not marked")
	}
}

func TestEntriesAndRuntimeReplayUseTerminalCommit(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = false

	if cmd := model.terminalCommit().Entries(sysEntry("notice")); cmd == nil {
		t.Fatal("terminal entries commit returned nil command")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("terminal entries commit did not mark transcript printed")
	}

	model.App.PrintedTranscript = false
	msg := runtimeSwitchedMsg{
		runtime: Accepted{
			Transition: Transition{
				Snapshot: Snapshot{},
			},
		},
		printLines: []string{"ion v0.0.0", "--- resumed ---"},
	}
	cmds := model.runtimeSwitchedCommands(msg)
	if len(cmds) == 0 {
		t.Fatal("runtimeSwitchedCommands returned no commands")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("runtime replay did not mark transcript printed")
	}
}

func TestPersistenceControllerAppendsEntriesAndReportsErrors(t *testing.T) {
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner

	cmd := model.persistenceController().appendEntry("persist test", StoreSystem{
		Type:    "system",
		Content: "hello",
	})
	if cmd == nil {
		t.Fatal("appendEntry returned nil for storage-backed model")
	}
	if msg := cmd(); msg != nil {
		t.Fatalf("appendEntry message = %#v, want nil", msg)
	}
	if len(runner.appends) != 1 {
		t.Fatalf("appends = %#v, want one append", runner.appends)
	}

	runner.appendErr = errors.New("disk full")
	cmd = model.persistenceController().appendEntry("persist test", StoreSystem{
		Type:    "system",
		Content: "failed",
	})
	msg := cmd()
	localErr, ok := msg.(localErrorMsg)
	if !ok {
		t.Fatalf("appendEntry message = %#v, want localErrorMsg", msg)
	}
	if !strings.Contains(localErr.err.Error(), "persist test: disk full") {
		t.Fatalf("local error = %v, want wrapped append error", localErr.err)
	}
}

func TestPersistenceControllerReturnsNilWithoutRunner(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = nil

	if cmd := model.persistenceController().appendEntry("persist test", StoreSystem{}); cmd != nil {
		t.Fatalf("appendEntry command = %#v, want nil without storage", cmd)
	}
}
