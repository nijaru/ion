package app

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/agent"

	"github.com/nijaru/ion/session"
)

func TestCanceledFrontendTimersStopWithoutMessages(t *testing.T) {
	model := Model{}
	model.Model.runtimeContext, model.Model.runtimeCancel = context.WithCancel(context.Background())
	model.Model.runtimeCancel()

	if msg := model.pollGitBranch()(); msg != nil {
		t.Fatalf("canceled branch poll message = %#v, want nil", msg)
	}
	if cmd := loadGitDiffStats(model.runtimeOperationContext(), t.TempDir()); cmd() != nil {
		t.Fatal("canceled git diff command returned a message")
	}
}

func TestModelCloseCancelsOwnedFrontendWork(t *testing.T) {
	model := Model{}
	model.Model.runtimeContext, model.Model.runtimeCancel = context.WithCancel(context.Background())
	model.Model.runtimeRequestContext, model.Model.runtimeRequestCancel = context.WithCancel(model.Model.runtimeContext)
	treeContext, treeCancel := context.WithCancel(context.Background())
	model.Model.treeNavigationCancel = treeCancel
	pickerContext, pickerCancel := context.WithCancel(context.Background())
	model.Picker.Overlay = &pickerOverlayState{
		loadContext: pickerContext,
		loadCancel:  pickerCancel,
	}
	turnState, turnContext := newTurnCancellationState(context.Background())
	model.Model.turnCancellation = turnState

	model.Close()
	model.Close()

	if err := model.Model.runtimeContext.Err(); !errors.Is(err, context.Canceled) {
		t.Fatalf("runtime context error = %v, want context canceled", err)
	}
	if err := model.Model.runtimeRequestContext.Err(); !errors.Is(err, context.Canceled) {
		t.Fatalf("runtime request context error = %v, want context canceled", err)
	}
	if err := treeContext.Err(); !errors.Is(err, context.Canceled) {
		t.Fatalf("tree navigation context error = %v, want context canceled", err)
	}
	if err := pickerContext.Err(); !errors.Is(err, context.Canceled) {
		t.Fatalf("picker context error = %v, want context canceled", err)
	}
	if model.Picker.Overlay != nil {
		t.Fatal("picker overlay remained installed after model close")
	}
	select {
	case <-turnContext.Done():
	default:
		t.Fatal("turn context remained active after model close")
	}
	if model.Model.turnCancellation != nil {
		t.Fatal("turn cancellation state remained installed after model close")
	}
}

func TestFormatPrintLinesAppendsSingleTrailingBlankLine(t *testing.T) {
	got := formatPrintLines("• answer", "", "")
	if !strings.HasSuffix(got, "\n") {
		t.Fatalf("formatted print body = %q, want trailing newline", got)
	}
	if strings.HasSuffix(got, "\n\n") {
		t.Fatalf("formatted print body = %q, want only a single trailing newline", got)
	}
	if got != "• answer\n" {
		t.Fatalf("formatted print body = %q, want trailing blanks trimmed with a single trailing newline", got)
	}
}

func TestFormatPrintLinesPreservesInteriorBlankLine(t *testing.T) {
	got := formatPrintLines("• first", "", "• second")
	if !strings.Contains(got, "\x1b[0m") {
		t.Fatalf("formatted print body = %q, want reset marker for interior blank line", got)
	}
	if !strings.HasSuffix(got, "\n") {
		t.Fatalf("formatted print body = %q, want trailing newline", got)
	}
	if strings.HasSuffix(got, "\n\n") {
		t.Fatalf("formatted print body = %q, want only a single trailing newline", got)
	}
}

func TestActionRecoverySummaryRequiresVerification(t *testing.T) {
	actions := []session.ActionRecord{
		{ID: "action-1", Tool: "bash", State: session.ActionIndeterminate, Error: "provider\nclosed before result"},
		{ID: "action-2", Tool: "mcp.fetch", State: session.ActionStarted},
	}

	got := actionRecoverySummary(actions)
	if !strings.Contains(got, "Action recovery: 2 unsettled external action(s); verify before retry") {
		t.Fatalf("summary = %q, want verification warning", got)
	}
	if !strings.Contains(got, "- action-1: bash indeterminate — provider closed before result") {
		t.Fatalf("summary = %q, want normalized action reason", got)
	}
	if !strings.Contains(got, "- action-2: mcp.fetch started") {
		t.Fatalf("summary = %q, want second action", got)
	}
	if actionRecoverySummary(nil) != "" {
		t.Fatal("empty recovery should not add a summary")
	}
}

func TestDebugCommandUsesRuntimeOperationContext(t *testing.T) {
	model := readyModel(t)
	runner := &projectionTestRunner{
		stubRunner: &stubRunner{},
		projection: agent.SessionProjection{ID: "debug-session"},
	}
	model.Model.Runner = runner
	expectedContext := model.Model.runtimeContext

	updated, cmd := model.handleDebugCommand()
	if cmd == nil {
		t.Fatal("debug command returned no command")
	}
	result, ok := cmd().(debugLogWrittenMsg)
	if !ok || result.generation != model.Model.EventGeneration || result.err != nil {
		t.Fatalf("debug result = %#v", result)
	}
	if runner.projectionCtx != expectedContext {
		t.Fatalf("debug projection context = %v, want accepted runtime context", runner.projectionCtx)
	}
	if _, err := os.Stat(result.path); err != nil {
		t.Fatalf("debug log %q: %v", result.path, err)
	}
	if _, cmd := updated.update(result); cmd == nil {
		t.Fatal("current debug completion did not render a terminal notice")
	}
}

func TestStaleDebugResultCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd := model.update(debugLogWrittenMsg{
		generation: 1,
		path:       "/old-runtime/.ion/debug.log",
		err:        errors.New("old runtime debug failed"),
	})
	if cmd != nil {
		t.Fatal("stale debug result returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale debug result rendered into the new runtime")
	}
}

func TestDebugCommandStopsBeforeWritingAfterCancellation(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &projectionTestRunner{
		stubRunner: &stubRunner{},
		projection: agent.SessionProjection{ID: "debug-session"},
	}
	home := t.TempDir()
	t.Setenv("HOME", home)
	model.Model.runtimeCancel()

	_, cmd := model.handleDebugCommand()
	if cmd == nil {
		t.Fatal("debug command returned no command")
	}
	result, ok := cmd().(debugLogWrittenMsg)
	if !ok || !errors.Is(result.err, context.Canceled) {
		t.Fatalf("canceled debug result = %#v", result)
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "debug.log")); !os.IsNotExist(err) {
		t.Fatalf("canceled debug wrote a log, stat error = %v", err)
	}
}
