package app

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/testutil"
)

func TestComposerLayoutResetsAfterClear(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("one\ntwo\nthree")
	model.layout()

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("expected composer to be cleared, got %q", got)
	}
	if got := model.Input.Composer.Height(); got != minComposerHeight {
		t.Fatalf("expected composer height to reset to %d, got %d", minComposerHeight, got)
	}
}

func TestHeaderShortenHomePathRequiresPathBoundary(t *testing.T) {
	home := filepath.Join(string(filepath.Separator), "Users", "nick")
	if got := shortenHomePath(filepath.Join(home, "repo"), home); got != filepath.Join("~", "repo") {
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
		model = updated.(Model)
	}

	if got := model.Input.Composer.Value(); got != "/help" {
		t.Fatalf("composer = %q, want %q", got, "/help")
	}
}

func TestEnterSubmitsSlashCommandFromComposer(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/help")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

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
	model = updated.(Model)

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

func TestDeferredEnterSubmitsAfterPrintHold(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/session")
	model.Input.DeferredEnter = true
	model.Input.PrintHoldUntil = time.Now().Add(-time.Millisecond)

	updated, cmd := model.Update(deferredEnterMsg{})
	model = updated.(Model)

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
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("first ctrl+c should arm quit timeout")
	}
	if !model.Input.CtrlCPending {
		t.Fatal("expected ctrlCPending after first ctrl+c")
	}
	if line := ansi.Strip(model.statusLine()); !strings.Contains(
		line,
		"Press Ctrl+C again to quit",
	) {
		t.Fatalf("status line = %q, want ctrl+c hint", line)
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = updated.(Model)
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
	model = updated.(Model)
	if cmd != nil {
		t.Fatal("ctrl+c with text should clear, not quit")
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared", got)
	}
	if model.Input.CtrlCPending {
		t.Fatal("ctrlCPending should remain false after clearing composer")
	}
}

func TestCtrlCIgnoredWhileRunning(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = updated.(Model)
	if cmd != nil {
		t.Fatal("ctrl+c while running should not quit")
	}
	if model.Input.CtrlCPending {
		t.Fatal("ctrlCPending should remain false while running")
	}
	if sess.cancels != 0 {
		t.Fatalf("cancel count = %d, want 0", sess.cancels)
	}
}

func TestCtrlDDoubleTapQuitsOnlyWhenIdleAndEmpty(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("first ctrl+d should arm quit timeout")
	}
	if !model.Input.CtrlCPending {
		t.Fatal("expected ctrlCPending after first ctrl+d")
	}
	if line := ansi.Strip(model.statusLine()); !strings.Contains(
		line,
		"Press Ctrl+D again to quit",
	) {
		t.Fatalf("status line = %q, want ctrl+d hint", line)
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'd', Mod: tea.ModCtrl})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("second ctrl+d should quit")
	}
	if _, ok := cmd().(tea.QuitMsg); !ok {
		t.Fatalf("second ctrl+d cmd = %T, want tea.QuitMsg", cmd())
	}
}

func TestEscCancelsRunningTurn(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	stored := &stubStorageSession{}
	model := New(stubBackend{sess: sess}, stored, nil, "/tmp/test", "main", "dev", nil)
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("esc while running should print durable cancellation")
	}
	if sess.cancels != 1 {
		t.Fatalf("cancel count = %d, want 1", sess.cancels)
	}
	if model.InFlight.Thinking {
		t.Fatal("thinking should be false after esc cancel")
	}
	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want unchanged", got)
	}
	if len(stored.appends) != 1 {
		t.Fatalf("appends = %#v, want one cancellation entry", stored.appends)
	}
	system, ok := stored.appends[0].(storage.System)
	if !ok || system.Content != "Canceled by user" {
		t.Fatalf("append = %#v, want cancellation system entry", stored.appends[0])
	}
}

func TestPendingActionTimeoutClearsStatusHint(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected timeout cmd after first ctrl+c")
	}

	updated, _ = model.Update(clearPendingMsg{action: pendingActionQuitCtrlC})
	model = updated.(Model)
	if model.Input.CtrlCPending || model.Input.Pending != pendingActionNone {
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
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "first\nsecond\nthird" {
		t.Fatalf("expected recalled history entry, got %q", got)
	}
	if got := model.Input.Composer.Height(); got != 3 {
		t.Fatalf("expected composer height to expand to 3, got %d", got)
	}
}

func TestCtrlTOpensThinkingPicker(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(tea.KeyPressMsg{Code: 't', Mod: tea.ModCtrl})
	model = updated.(Model)

	if model.Picker.Overlay == nil {
		t.Fatal("expected thinking picker to open")
	}
	if model.Picker.Overlay.purpose != pickerPurposeThinking {
		t.Fatalf("picker purpose = %v, want thinking", model.Picker.Overlay.purpose)
	}
	if got := model.Picker.Overlay.title; got != "Pick a primary thinking level" {
		t.Fatalf("picker title = %q", got)
	}
	var values []string
	for _, item := range model.Picker.Overlay.items {
		values = append(values, item.Value)
	}
	want := []string{"auto", "off", "minimal", "low", "medium", "high", "xhigh"}
	if !slices.Equal(values, want) {
		t.Fatalf("thinking picker values = %#v, want %#v", values, want)
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

func TestCtrlXControlTextDoesNotEnterComposerWhileBusy(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Text: "\x18", Code: 'x', Mod: tea.ModCtrl})
	model = updated.(Model)

	if cmd == nil {
		t.Fatal("busy editor handoff should print a notice")
	}
	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want draft without control character", got)
	}
}

func TestCtrlPRecallsHistory(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"first", "second"}

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	model = updated.(Model)
	if got := model.Input.Composer.Value(); got != "second" {
		t.Fatalf("composer = %q, want latest history entry", got)
	}

	updated, _ = model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	model = updated.(Model)
	if got := model.Input.Composer.Value(); got != "first" {
		t.Fatalf("composer = %q, want previous history entry", got)
	}
}

func TestCtrlNTogglesForwardThroughHistory(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"first", "second"}

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	model = updated.(Model)
	updated, _ = model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	model = updated.(Model)

	updated, _ = model.Update(tea.KeyPressMsg{Code: 'n', Mod: tea.ModCtrl})
	model = updated.(Model)
	if got := model.Input.Composer.Value(); got != "second" {
		t.Fatalf("composer = %q, want next history entry", got)
	}
}

func TestCtrlMTogglesPrimaryAndFastPreset(t *testing.T) {
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
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			observedModels = append(observedModels, cfg.Model)
			resolved := *cfg
			newBackend := testutil.New()
			newBackend.SetConfig(&resolved)
			newStorage := &stubStorageSession{
				id:     sessionID,
				model:  cfg.Provider + "/" + cfg.Model,
				branch: "main",
			}
			newBackend.SetSession(newStorage)
			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'm', Mod: tea.ModCtrl})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected ctrl+m to return a switch command")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ := model.Update(switched)
	model = next.(Model)
	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast", model.App.ActivePreset)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("state active_preset = %#v, want fast", state.ActivePreset)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1-mini" {
		t.Fatalf("fast model = %q, want gpt-4.1-mini", got)
	}
	if got := model.Progress.ReasoningEffort; got != "low" {
		t.Fatalf("fast reasoning = %q, want low", got)
	}

	updated, cmd = model.Update(tea.KeyPressMsg{Code: 'm', Mod: tea.ModCtrl})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected ctrl+m to switch back to primary")
	}
	msg = cmd()
	switched, ok = msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ = model.Update(switched)
	model = next.(Model)
	if model.App.ActivePreset != presetPrimary {
		t.Fatalf("active preset = %q, want primary", model.App.ActivePreset)
	}
	state, err = config.LoadState()
	if err != nil {
		t.Fatalf("load state after primary switch: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "primary" {
		t.Fatalf("state active_preset = %#v, want primary", state.ActivePreset)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("primary model = %q, want gpt-4.1", got)
	}
	if !slices.Equal(observedModels, []string{"gpt-4.1-mini", "gpt-4.1"}) {
		t.Fatalf("switched models = %#v, want fast then primary", observedModels)
	}
}
