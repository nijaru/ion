package app

import (
	"context"
	"errors"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/session"
)

func TestComposerLayoutResetsAfterClear(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("one\ntwo\nthree")
	model.layout()

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)

	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("expected composer to be cleared, got %q", got)
	}
	if got := model.Input.Composer.Height(); got != minComposerHeight {
		t.Fatalf("expected composer height to reset to %d, got %d", minComposerHeight, got)
	}
}

func TestHeaderShortenHomePathRequiresPathBoundary(t *testing.T) {
	home := filepath.Join(string(filepath.Separator), "Users", "nick")
	if got := shortenHomePath(filepath.Join(home, "repo"), home); got != filepath.Join(
		"~",
		"repo",
	) {
		t.Fatalf("shortened home path = %q, want ~/repo", got)
	}
	sibling := filepath.Join(string(filepath.Separator), "Users", "nick2", "repo")
	if got := shortenHomePath(sibling, home); got != sibling {
		t.Fatalf("sibling path = %q, want unshortened %q", got, sibling)
	}
}

func TestComposerAcceptsTypedText(t *testing.T) {
	model := readyModel(t)

	for _, key := range []tea.KeyPressMsg{
		{Text: "/", Code: '/'},
		{Text: "h", Code: 'h'},
		{Text: "e", Code: 'e'},
		{Text: "l", Code: 'l'},
		{Text: "p", Code: 'p'},
	} {
		updated, _ := model.Update(key)
		model = testModel(t, updated)
	}

	if got := model.Input.Composer.Value(); got != "/help" {
		t.Fatalf("composer = %q, want %q", got, "/help")
	}
}

func TestNewLoadsPersistedInputHistoryForRecall(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	ctx := context.Background()
	store, err := session.NewSQLiteStore(filepath.Join(t.TempDir(), "ion.db"), "input-test")
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	defer store.Close()
	var inputStore inputHistoryStore = store
	cwd := t.TempDir()
	for _, input := range []string{"first prompt", "second prompt"} {
		if err := inputStore.AddInput(ctx, cwd, input); err != nil {
			t.Fatalf("add input: %v", err)
		}
	}

	model := New(
		stubBackend{
			sess:     &stubSession{events: make(chan session.Event)},
			provider: "fake",
			model:    "model",
		},
		nil,
		store,
		cwd,
		"main",
		"dev",
		nil,
	)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 80, Height: 30})
	model = testModel(t, updated)

	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = testModel(t, updated)
	if got := model.Input.Composer.Value(); got != "second prompt" {
		t.Fatalf("composer = %q, want latest persisted input", got)
	}
}

func TestSubmitTextPersistsInputHistory(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	ctx := context.Background()
	store, err := session.NewSQLiteStore(filepath.Join(t.TempDir(), "ion.db"), "input-test")
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	defer store.Close()
	var inputStore inputHistoryStore = store
	cwd := t.TempDir()
	model := New(
		stubBackend{
			sess:     &stubSession{events: make(chan session.Event)},
			provider: "fake",
			model:    "model",
		},
		nil,
		store,
		cwd,
		"main",
		"dev",
		nil,
	)

	_, cmd := model.submitText("/help")
	if cmd == nil {
		t.Fatal("expected input history command")
	}
	runCommandTree(t, cmd)

	inputs, err := inputStore.GetInputs(ctx, cwd, 1)
	if err != nil {
		t.Fatalf("get inputs: %v", err)
	}
	if len(inputs) != 1 || inputs[0] != "/help" {
		t.Fatalf("inputs = %#v, want persisted slash command", inputs)
	}
}

type blockingInputStore struct {
	resumeOnlyStore
	started chan struct{}
	release chan struct{}
}

func (s *blockingInputStore) AddInput(ctx context.Context, cwd, content string) error {
	close(s.started)
	<-s.release
	return nil
}

func TestPersistInputHistoryReturnsBeforeStoreWriteCompletes(t *testing.T) {
	store := &blockingInputStore{
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	model := readyModel(t)
	model.Model.Store = store
	model.App.Workdir = t.TempDir()

	returned := make(chan tea.Cmd, 1)
	go func() {
		returned <- model.persistInputHistory(context.Background(), "hello")
	}()

	var cmd tea.Cmd
	select {
	case cmd = <-returned:
	case <-time.After(2 * time.Second):
		t.Fatal("persistInputHistory blocked on store write")
	}
	if cmd == nil {
		t.Fatal("persistInputHistory returned nil command")
	}
	select {
	case <-store.started:
		t.Fatal("input history write ran before Bubble Tea command execution")
	default:
	}

	done := make(chan tea.Msg, 1)
	go func() {
		done <- cmd()
	}()
	select {
	case <-store.started:
	case <-time.After(2 * time.Second):
		t.Fatal("input history command did not write to store")
	}
	select {
	case msg := <-done:
		t.Fatalf("input history command returned before store write completed: %T", msg)
	default:
	}

	close(store.release)
	if msg := <-done; msg != nil {
		t.Fatalf("input history command result = %T, want nil", msg)
	}
}

func TestEnterSubmitsSlashCommandFromComposer(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/help")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after submit", got)
	}
	if cmd == nil {
		t.Fatal("expected slash command print command")
	}
}

func TestEnterDuringLargePrintHoldDefersSubmission(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/session")
	model.holdEnterForLargePrint(40)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

	if !model.Input.DeferredEnter {
		t.Fatal("expected Enter to be deferred while large print is flushing")
	}
	if got := model.Input.Composer.Value(); got != "/session" {
		t.Fatalf("composer = %q, want deferred command to remain editable", got)
	}
	if cmd == nil {
		t.Fatal("expected deferred Enter timer command")
	}
}

func TestEnterDuringRuntimeSwitchLeavesDraftAndOldSessionAlone(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Storage = sess
	model.Model.RuntimeSwitchRequest = 1
	model.Input.Composer.SetValue("run this after the switch")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("expected runtime-switch guard error")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "runtime switch") {
		t.Fatalf("error = %v, want runtime switch guard", err)
	}
	if got := model.Input.Composer.Value(); got != "run this after the switch" {
		t.Fatalf("composer = %q, want draft preserved", got)
	}
	if len(sess.submits) != 0 {
		t.Fatalf("old session submits = %#v, want none", sess.submits)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want none", model.InFlight.QueuedTurns)
	}
}

func TestDeferredEnterSubmitsAfterPrintHold(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/session")
	model.Input.DeferredEnter = true
	model.Input.PrintHoldUntil = time.Now().Add(-time.Millisecond)

	updated, cmd := model.Update(deferredEnterMsg{})
	model = testModel(t, updated)

	if model.Input.DeferredEnter {
		t.Fatal("expected deferred Enter state to clear after submit")
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after deferred submit", got)
	}
	if cmd == nil {
		t.Fatal("expected deferred slash command print command")
	}
}

func TestCtrlCDoubleTapQuitsOnlyWhenIdleAndEmpty(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("first ctrl+c should arm quit timeout")
	}
	if model.Input.Pending != pendingActionQuitCtrlC {
		t.Fatalf("pending action = %v, want ctrl+c quit", model.Input.Pending)
	}
	if line := ansi.Strip(model.statusLine()); !strings.Contains(
		line,
		"Press Ctrl+C again to quit",
	) {
		t.Fatalf("status line = %q, want ctrl+c hint", line)
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("second ctrl+c should quit")
	}
	if _, ok := cmd().(tea.QuitMsg); !ok {
		t.Fatalf("second ctrl+c cmd = %T, want tea.QuitMsg", cmd())
	}
}

func TestCtrlCClearsComposerWithoutArmingQuit(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd != nil {
		t.Fatal("ctrl+c with text should clear, not quit")
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared", got)
	}
	if model.Input.Pending != pendingActionNone {
		t.Fatal("pending action should remain clear after clearing composer")
	}
}

func TestCtrlCCancelsRunningTurn(t *testing.T) {
	// Pi parity: Ctrl+C clears editor, Escape cancels running turn.
	// This test verifies Escape cancels running turn.
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner
	model.InFlight.Thinking = true
	model.InFlight.QueuedTurns = []string{"follow up"}

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("escape while running should print durable cancellation")
	}
	if model.Input.Pending != pendingActionNone {
		t.Fatal("pending action should remain clear while running")
	}
	if runner.aborts != 0 {
		t.Fatalf("cancel count before command execution = %d, want 0", runner.aborts)
	}
	if !model.InFlight.Thinking || !model.InFlight.Canceling {
		t.Fatalf(
			"cancel state = thinking %v canceling %v, want true/true",
			model.InFlight.Thinking,
			model.InFlight.Canceling,
		)
	}
	if model.Progress.Mode != StateCancelled {
		t.Fatalf("progress mode = %v, want StateCancelled", model.Progress.Mode)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want cleared", model.InFlight.QueuedTurns)
	}
	runCommandTree(t, cmd)
	if runner.aborts != 1 {
		t.Fatalf("cancel count after command execution = %d, want 1", runner.aborts)
	}
}

func TestCtrlCClearsComposerBeforeCancelingRunningTurn(t *testing.T) {
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("draft follow-up")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd != nil {
		t.Fatal("ctrl+c with a draft should clear composer, not cancel")
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared", got)
	}
	if !model.InFlight.Thinking {
		t.Fatal("turn should keep running after clearing draft")
	}
	if runner.aborts != 0 {
		t.Fatalf("cancel count = %d, want 0", runner.aborts)
	}
}

func TestCtrlDDoubleTapQuitsOnlyWhenIdleAndEmpty(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("first ctrl+d should arm quit timeout")
	}
	if model.Input.Pending != pendingActionQuitCtrlD {
		t.Fatalf("pending action = %v, want ctrl+d quit", model.Input.Pending)
	}
	if line := ansi.Strip(model.statusLine()); !strings.Contains(
		line,
		"Press Ctrl+D again to quit",
	) {
		t.Fatalf("status line = %q, want ctrl+d hint", line)
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("second ctrl+d should quit")
	}
	if _, ok := cmd().(tea.QuitMsg); !ok {
		t.Fatalf("second ctrl+d cmd = %T, want tea.QuitMsg", cmd())
	}
}

func TestQuitDoubleTapRequiresSameKey(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil || model.Input.Pending != pendingActionQuitCtrlC {
		t.Fatal("first ctrl+c should arm ctrl+c quit")
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("ctrl+d after ctrl+c should arm ctrl+d quit")
	}
	if model.Input.Pending != pendingActionQuitCtrlD {
		t.Fatalf("pending action = %v, want ctrl+d quit", model.Input.Pending)
	}

	model = readyModel(t)
	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil || model.Input.Pending != pendingActionQuitCtrlD {
		t.Fatal("first ctrl+d should arm ctrl+d quit")
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("ctrl+c after ctrl+d should arm ctrl+c quit")
	}
	if model.Input.Pending != pendingActionQuitCtrlC {
		t.Fatalf("pending action = %v, want ctrl+c quit", model.Input.Pending)
	}
}

func TestCtrlDWithDraftEditsComposer(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("abc")
	model.Input.Composer.CursorStart()

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd != nil {
		t.Fatalf("ctrl+d with draft returned cmd %T, want composer edit only", cmd)
	}
	if got := model.Input.Composer.Value(); got != "bc" {
		t.Fatalf("composer = %q, want delete-forward result", got)
	}
	if model.Input.Pending != pendingActionNone {
		t.Fatal("ctrl+d with draft should not arm quit")
	}
}

func TestCtrlDIgnoredWhileRunning(t *testing.T) {
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd != nil {
		t.Fatal("ctrl+d while running should not quit or print")
	}
	if model.Input.Pending != pendingActionNone {
		t.Fatal("ctrl+d while running should not arm quit")
	}
	if !model.InFlight.Thinking {
		t.Fatal("ctrl+d should not cancel running turn")
	}
	if runner.aborts != 0 {
		t.Fatalf("cancel count = %d, want 0", runner.aborts)
	}
}

func TestEscCancelsRunningTurn(t *testing.T) {
	runner := &stubRunner{}
	stored := &stubStorageSession{}
	model := New(stubBackend{}, stored, nil, "/tmp/test", "main", "dev", nil)
	model.Model.Runner = runner
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("esc while running should print durable cancellation")
	}
	if runner.aborts != 0 {
		t.Fatalf("cancel count before command execution = %d, want 0", runner.aborts)
	}
	if !model.InFlight.Thinking || !model.InFlight.Canceling {
		t.Fatalf(
			"cancel state = thinking %v canceling %v, want true/true",
			model.InFlight.Thinking,
			model.InFlight.Canceling,
		)
	}
	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want unchanged", got)
	}
	if len(runner.appends) != 0 {
		t.Fatalf("appends before command execution = %#v, want none", runner.appends)
	}
	runCommandTree(t, cmd)
	if len(runner.appends) != 1 {
		t.Fatalf(
			"appends after command execution = %#v, want one cancellation entry",
			runner.appends,
		)
	}
	system, ok := runner.appends[0].(*session.CustomEntry)
	if !ok || system.Type != "store_system" {
		t.Fatalf("append = %#v, want store_system CustomEntry", runner.appends[0])
	}
	if runner.aborts != 1 {
		t.Fatalf("cancel count after command execution = %d, want 1", runner.aborts)
	}
}

func TestPendingActionTimeoutClearsStatusHint(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected timeout cmd after first ctrl+c")
	}

	updated, _ = model.Update(clearPendingMsg{action: pendingActionQuitCtrlC})
	model = testModel(t, updated)
	if model.Input.Pending != pendingActionNone {
		t.Fatal("pending action should clear after timeout")
	}
	if line := ansi.Strip(model.statusLine()); strings.Contains(
		line,
		"Press Ctrl+C again to quit",
	) {
		t.Fatalf("status line should clear timeout hint, got %q", line)
	}
}

func TestComposerLayoutReflowsAfterHistoryRecall(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"first\nsecond\nthird"}

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = testModel(t, updated)

	if got := model.Input.Composer.Value(); got != "first\nsecond\nthird" {
		t.Fatalf("expected recalled history entry, got %q", got)
	}
	if got := model.Input.Composer.Height(); got != 3 {
		t.Fatalf("expected composer height to expand to 3, got %d", got)
	}
}

func TestExternalEditorFinishedUpdatesComposer(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("[paste #1 +12 lines]")
	model.PasteMarkers["[paste #1 +12 lines]"] = pasteMarker{
		placeholder: "[paste #1 +12 lines]",
		content:     "expanded paste",
	}

	updated, cmd := model.handleExternalEditorFinished(externalEditorFinishedMsg{
		content: "edited\nmessage",
	})

	if cmd != nil {
		t.Fatal("editor finish should not emit a command on success")
	}
	if got := updated.Input.Composer.Value(); got != "edited\nmessage" {
		t.Fatalf("composer = %q, want edited content", got)
	}
	if len(updated.PasteMarkers) != 0 {
		t.Fatalf("paste markers = %#v, want cleared", updated.PasteMarkers)
	}
	if got := updated.Input.Composer.Height(); got != 2 {
		t.Fatalf("composer height = %d, want 2", got)
	}
}

func TestExternalEditorReturnsBeforeBufferWriteCompletes(t *testing.T) {
	previousWrite := writeExternalEditorBufferFile
	previousEditor := externalEditorName
	t.Cleanup(func() {
		writeExternalEditorBufferFile = previousWrite
		externalEditorName = previousEditor
	})

	writeStarted := make(chan string, 1)
	releaseWrite := make(chan struct{})
	writeExternalEditorBufferFile = func(content string) (string, error) {
		writeStarted <- content
		<-releaseWrite
		return "", errors.New("write failed")
	}
	externalEditorName = func() string {
		return "false"
	}

	model := readyModel(t)
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'g', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("external editor should return a command")
	}
	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want unchanged draft", got)
	}
	select {
	case content := <-writeStarted:
		t.Fatalf("buffer write ran before Bubble Tea command execution with content %q", content)
	default:
	}

	done := make(chan tea.Msg, 1)
	go func() {
		done <- cmd()
	}()
	select {
	case content := <-writeStarted:
		if content != "draft" {
			t.Fatalf("buffer content = %q, want draft", content)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("external editor command did not start buffer write")
	}
	select {
	case msg := <-done:
		t.Fatalf("external editor command returned before buffer write completed: %T", msg)
	default:
	}

	close(releaseWrite)
	msg := <-done
	finished, ok := msg.(externalEditorFinishedMsg)
	if !ok {
		t.Fatalf("command result = %T, want externalEditorFinishedMsg", msg)
	}
	if finished.err == nil || !strings.Contains(finished.err.Error(), "write failed") {
		t.Fatalf("command err = %v, want write failed", finished.err)
	}
}

func TestExternalEditorUsesVisualBeforeEditor(t *testing.T) {
	t.Setenv("VISUAL", "code --wait")
	t.Setenv("EDITOR", "vim")

	if got := externalEditor(); got != "code --wait" {
		t.Fatalf("external editor = %q, want VISUAL", got)
	}
}

func TestWriteExternalEditorBuffer(t *testing.T) {
	path, err := writeExternalEditorBuffer("draft")
	if err != nil {
		t.Fatalf("write editor buffer: %v", err)
	}
	defer os.Remove(path)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read editor buffer: %v", err)
	}
	if string(data) != "draft" {
		t.Fatalf("buffer = %q, want draft", data)
	}
}

func TestCtrlGExternalEditorDoesNotEnterComposerWhileBusy(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'g', Mod: tea.ModCtrl})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("external editor should open even while busy")
	}
	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want draft without control character", got)
	}
}

func TestUpArrowRecallsHistory(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"first", "second"}

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = testModel(t, updated)
	if got := model.Input.Composer.Value(); got != "second" {
		t.Fatalf("composer = %q, want latest history entry", got)
	}

	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = testModel(t, updated)
	if got := model.Input.Composer.Value(); got != "first" {
		t.Fatalf("composer = %q, want previous history entry", got)
	}
}

func TestDownArrowTogglesForwardThroughHistory(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"first", "second"}

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = testModel(t, updated)
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = testModel(t, updated)

	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = testModel(t, updated)
	if got := model.Input.Composer.Value(); got != "second" {
		t.Fatalf("composer = %q, want next history entry", got)
	}
}

func TestCtrlLCyclesPrimaryAndFastPreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte(
		"provider = \"openai\"\nmodel = \"gpt-4.1\"\nreasoning_effort = \"auto\"\nfast_model = \"gpt-4.1-mini\"\n",
	), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{sess: oldSession, provider: "openai", model: "gpt-4.1"}

	var observedModels []string
	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (RuntimeInfo, agent.Runner, session.Session, error) {
			observedModels = append(observedModels, cfg.Model)
			resolved := *cfg
			newBackend := stubBackend{provider: resolved.Provider, model: resolved.Model}
			newStorage := &stubStorageSession{
				storageID:     sessionID,
				storageModel:  cfg.Provider + "/" + cfg.Model,
				storageBranch: "main",
			}
			return newBackend, &stubRunner{}, newStorage, nil
		},
	)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected ctrl+l to return a switch command")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ := model.Update(switched)
	model = testModel(t, next)
	// With available model cycling, preset stays primary but model changes
	if model.App.ActivePreset != PresetPrimary {
		t.Fatalf("active preset = %q, want primary", model.App.ActivePreset)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1-mini" {
		t.Fatalf("model = %q, want gpt-4.1-mini", got)
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected ctrl+l to switch back to primary")
	}
	msg = cmd()
	switched, ok = msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ = model.Update(switched)
	model = testModel(t, next)
	if model.App.ActivePreset != PresetPrimary {
		t.Fatalf("active preset = %q, want primary", model.App.ActivePreset)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("model = %q, want gpt-4.1", got)
	}
	if !slices.Equal(observedModels, []string{"gpt-4.1-mini", "gpt-4.1"}) {
		t.Fatalf("switched models = %#v, want fast then primary", observedModels)
	}
}

func TestCtrlLBlockedDuringBusyTurn(t *testing.T) {
	oldSession := &stubSession{events: make(chan session.Event)}
	model := New(
		stubBackend{sess: oldSession, provider: "openai", model: "gpt-4.1"},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (RuntimeInfo, agent.Runner, session.Session, error) {
			t.Fatal("busy preset toggle should not switch runtimes")
			return nil, nil, nil, nil
		},
	).WithConfig(&config.Config{
		Provider:  "openai",
		Model:     "gpt-4.1",
		FastModel: "gpt-4.1-mini",
	})
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected ctrl+l to return a busy-turn error")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "Finish or cancel the current turn") {
		t.Fatalf("error = %v, want busy-turn guard", err)
	}
	if oldSession.cancels != 0 {
		t.Fatalf("cancels = %d, want 0", oldSession.cancels)
	}
	if model.App.ActivePreset != PresetPrimary {
		t.Fatalf("active preset = %q, want primary", model.App.ActivePreset)
	}
}

func TestCtrlLCyclesScopedModelsForward(t *testing.T) {
	var observed []string
	model := readyModelWithSwitcher(t, &observed).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Provider: "openai", Model: "gpt-4.1"},
			{Provider: "anthropic", Model: "claude-sonnet-4-5"},
			{Provider: "openai", Model: "gpt-4.1-mini"},
		},
	})

	// Forward: gpt-4.1 → claude-sonnet-4-5
	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected ctrl+l to switch to next scoped model")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ := model.Update(switched)
	model = testModel(t, next)
	if got, want := model.Model.Backend.Model(), "claude-sonnet-4-5"; got != want {
		t.Fatalf("model = %q, want %q", got, want)
	}
	if got := model.Model.Backend.Provider(); got != "anthropic" {
		t.Fatalf("provider = %q, want anthropic", got)
	}

	// Forward: claude-sonnet-4-5 → gpt-4.1-mini
	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	msg = cmd()
	switched, ok = msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ = model.Update(switched)
	model = testModel(t, next)
	if got := model.Model.Backend.Model(); got != "gpt-4.1-mini" {
		t.Fatalf("model = %q, want gpt-4.1-mini", got)
	}

	// Forward: gpt-4.1-mini → gpt-4.1 (wraps)
	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	msg = cmd()
	switched, ok = msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ = model.Update(switched)
	model = testModel(t, next)
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("model = %q, want gpt-4.1 (wrap)", got)
	}
}

func TestCtrlLCyclesScopedModelsBackward(t *testing.T) {
	var observed []string
	model := readyModelWithSwitcher(t, &observed).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Provider: "openai", Model: "gpt-4.1"},
			{Provider: "anthropic", Model: "claude-sonnet-4-5"},
			{Provider: "openai", Model: "gpt-4.1-mini"},
		},
	})

	// Backward from gpt-4.1 wraps to gpt-4.1-mini
	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl | tea.ModShift})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected shift+ctrl+l to switch scoped model")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ := model.Update(switched)
	model = testModel(t, next)
	if got := model.Model.Backend.Model(); got != "gpt-4.1-mini" {
		t.Fatalf("model = %q, want gpt-4.1-mini (backward wrap)", got)
	}
}

func TestPickScopedModel(t *testing.T) {
	models := []config.ScopedModel{
		{Provider: "openai", Model: "a"},
		{Provider: "anthropic", Model: "b"},
		{Provider: "openai", Model: "c"},
	}

	// Forward from index 0 → 1
	next, ok := pickScopedModel(models, "openai", "a", true)
	if !ok || next.Model != "b" {
		t.Fatalf("forward from a: got %+v ok=%v, want b", next, ok)
	}

	// Forward from last index wraps to 0
	next, ok = pickScopedModel(models, "openai", "c", true)
	if !ok || next.Model != "a" {
		t.Fatalf("forward wrap from c: got %+v ok=%v, want a", next, ok)
	}

	// Backward from index 0 wraps to last
	next, ok = pickScopedModel(models, "openai", "a", false)
	if !ok || next.Model != "c" {
		t.Fatalf("backward wrap from a: got %+v ok=%v, want c", next, ok)
	}

	// Unknown current model defaults to index 0, then advances
	next, ok = pickScopedModel(models, "unknown", "xxx", true)
	if !ok || next.Model != "b" {
		t.Fatalf("unknown current: got %+v ok=%v, want b", next, ok)
	}

	// Single model → no cycling
	single := []config.ScopedModel{{Provider: "openai", Model: "a"}}
	_, ok = pickScopedModel(single, "openai", "a", true)
	if ok {
		t.Fatal("single scoped model should not cycle")
	}

	// Empty list → no cycling
	_, ok = pickScopedModel(nil, "openai", "a", true)
	if ok {
		t.Fatal("empty scoped models should not cycle")
	}
}

func TestBuildAvailableModels(t *testing.T) {
	model := readyModel(t)

	// Test with primary and fast models configured
	cfg := &config.Config{
		Provider:  "openai",
		Model:     "gpt-4.1",
		FastModel: "gpt-4.1-mini",
	}
	available := model.buildAvailableModels(cfg)
	if len(available) != 2 {
		t.Fatalf("expected 2 available models, got %d: %+v", len(available), available)
	}
	if available[0].Provider != "openai" || available[0].Model != "gpt-4.1" {
		t.Fatalf("first model = %s/%s, want openai/gpt-4.1", available[0].Provider, available[0].Model)
	}
	if available[1].Provider != "openai" || available[1].Model != "gpt-4.1-mini" {
		t.Fatalf("second model = %s/%s, want openai/gpt-4.1-mini", available[1].Provider, available[1].Model)
	}

	// Test with only primary model (no fast)
	cfg2 := &config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
	}
	available2 := model.buildAvailableModels(cfg2)
	if len(available2) != 1 {
		t.Fatalf("expected 1 available model, got %d: %+v", len(available2), available2)
	}

	// Test with same primary and fast model
	cfg3 := &config.Config{
		Provider:  "openai",
		Model:     "gpt-4.1",
		FastModel: "gpt-4.1",
	}
	available3 := model.buildAvailableModels(cfg3)
	if len(available3) != 1 {
		t.Fatalf("expected 1 available model when primary == fast, got %d: %+v", len(available3), available3)
	}

	// Test with originalPrimaryModel set (preserves list after cycling)
	model.Model.originalPrimaryModel = "gpt-4.1"
	cfg4 := &config.Config{
		Provider:  "openai",
		Model:     "gpt-4.1-mini", // Currently on fast model
		FastModel: "gpt-4.1-mini",
	}
	available4 := model.buildAvailableModels(cfg4)
	if len(available4) != 2 {
		t.Fatalf("expected 2 available models with originalPrimaryModel, got %d: %+v", len(available4), available4)
	}
	if available4[0].Model != "gpt-4.1" {
		t.Fatalf("first model = %s, want gpt-4.1 (original primary)", available4[0].Model)
	}
	if available4[1].Model != "gpt-4.1-mini" {
		t.Fatalf("second model = %s, want gpt-4.1-mini", available4[1].Model)
	}
}
