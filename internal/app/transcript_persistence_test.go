package app

import (
	"errors"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
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

	if cmd := model.terminalCommit().Entries(session.Entry{Role: session.System, Content: "notice"}); cmd == nil {
		t.Fatal("terminal entries commit returned nil command")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("terminal entries commit did not mark transcript printed")
	}

	model.App.PrintedTranscript = false
	msg := runtimeSwitchedMsg{
		runtime: acceptedRuntime{
			Transition: runtimeTransition{
				Snapshot: runtimeSnapshot{},
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
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	cmd := model.persistenceController().appendEntry("persist test", storage.System{
		Type:    "system",
		Content: "hello",
	})
	if cmd == nil {
		t.Fatal("appendEntry returned nil for storage-backed model")
	}
	if msg := cmd(); msg != nil {
		t.Fatalf("appendEntry message = %#v, want nil", msg)
	}
	if len(storageSess.appends) != 1 {
		t.Fatalf("appends = %#v, want one append", storageSess.appends)
	}

	storageSess.appendErr = errors.New("disk full")
	cmd = model.persistenceController().appendEntry("persist test", storage.System{
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

func TestPersistenceControllerReturnsNilWithoutStorage(t *testing.T) {
	model := readyModel(t)
	model.Model.Storage = nil

	if cmd := model.persistenceController().appendEntry("persist test", storage.System{}); cmd != nil {
		t.Fatalf("appendEntry command = %#v, want nil without storage", cmd)
	}
}
