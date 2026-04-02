package app

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/testutil"
)

type stubBackend struct {
	sess         *stubSession
	provider     string
	model        string
	providerSet  bool
	modelSet     bool
	contextLimit int
}

type compactBackend struct {
	stubBackend
	compacted bool
	err       error
	called    bool
}

func (b stubBackend) Name() string { return "stub" }
func (b stubBackend) Provider() string {
	if b.providerSet || b.provider != "" {
		return b.provider
	}
	return "stub"
}

func (b stubBackend) Model() string {
	if b.modelSet || b.model != "" {
		return b.model
	}
	return "stub-model"
}

func (b stubBackend) ContextLimit() int {
	if b.contextLimit != 0 {
		return b.contextLimit
	}
	return 0
}

func (b stubBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{{Role: session.System, Content: "boot"}},
		Status:  "ready",
	}
}

func (b stubBackend) Session() session.AgentSession { return b.sess }

func (b stubBackend) SetStore(s storage.Store) {}

func (b stubBackend) SetSession(s storage.Session) {}

func (b stubBackend) SetConfig(cfg *config.Config) {}

func (b *compactBackend) Compact(ctx context.Context) (bool, error) {
	b.called = true
	return b.compacted, b.err
}

type stubSession struct {
	events    chan session.Event
	submits   []string
	cancels   int
	submitErr error
}

func (s *stubSession) Open(ctx context.Context) error              { return nil }
func (s *stubSession) Resume(ctx context.Context, id string) error { return nil }
func (s *stubSession) SubmitTurn(ctx context.Context, turn string) error {
	if s.submitErr != nil {
		return s.submitErr
	}
	s.submits = append(s.submits, turn)
	return nil
}

func (s *stubSession) CancelTurn(ctx context.Context) error {
	s.cancels++
	return nil
}

func (s *stubSession) Close() error {
	if s.events != nil {
		close(s.events)
		s.events = nil
	}
	return nil
}
func (s *stubSession) Events() <-chan session.Event                          { return s.events }
func (s *stubSession) Approve(ctx context.Context, id string, ok bool) error { return nil }
func (s *stubSession) RegisterMCPServer(ctx context.Context, cmd string, args ...string) error {
	return nil
}
func (s *stubSession) SetMode(mode session.Mode) {}

func (s *stubSession) SetAutoApprove(bool)     {}
func (s *stubSession) AllowCategory(string)    {}
func (s *stubSession) ID() string              { return "stub" }
func (s *stubSession) Meta() map[string]string { return nil }

type stubStorageSession struct {
	id        string
	model     string
	branch    string
	closed    bool
	appends   []any
	appendErr error
	usageIn   int
	usageOut  int
	usageCost float64
}

func (s *stubStorageSession) ID() string { return s.id }

func (s *stubStorageSession) Meta() storage.Metadata {
	return storage.Metadata{
		ID:     s.id,
		Model:  s.model,
		Branch: s.branch,
	}
}

func (s *stubStorageSession) Append(ctx context.Context, event any) error {
	s.appends = append(s.appends, event)
	return s.appendErr
}

func (s *stubStorageSession) Entries(ctx context.Context) ([]session.Entry, error) {
	return nil, nil
}

func (s *stubStorageSession) LastStatus(ctx context.Context) (string, error) { return "", nil }

func (s *stubStorageSession) Usage(ctx context.Context) (int, int, float64, error) {
	return s.usageIn, s.usageOut, s.usageCost, nil
}

func (s *stubStorageSession) Close() error {
	s.closed = true
	return nil
}

type resumeOnlyStore struct {
	resumed storage.Session
}

func (s *resumeOnlyStore) OpenSession(
	ctx context.Context,
	cwd, model, branch string,
) (storage.Session, error) {
	return nil, nil
}

func (s *resumeOnlyStore) ResumeSession(ctx context.Context, id string) (storage.Session, error) {
	return s.resumed, nil
}

func (s *resumeOnlyStore) ListSessions(
	ctx context.Context,
	cwd string,
) ([]storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) GetRecentSession(
	ctx context.Context,
	cwd string,
) (*storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) AddInput(ctx context.Context, cwd, content string) error { return nil }

func (s *resumeOnlyStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	return nil, nil
}

func (s *resumeOnlyStore) UpdateSession(ctx context.Context, si storage.SessionInfo) error {
	return nil
}

func (s *resumeOnlyStore) Close() error { return nil }

func readyModel(t *testing.T) Model {
	t.Helper()
	sess := &stubSession{events: make(chan session.Event)}
	b := stubBackend{sess: sess}
	model := New(b, nil, nil, "/tmp/test", "main", "dev", nil)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	ready, ok := updated.(Model)
	if !ok {
		t.Fatalf("expected Model after window size update")
	}
	return ready
}

func TestModelStreamsAndCommitsPendingEntry(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.AgentDelta{Delta: "streamed reply"})
	model = updated.(Model)

	if model.InFlight.Pending == nil || model.InFlight.Pending.Content != "streamed reply" {
		t.Fatalf("expected pending streamed agent entry, got %#v", model.InFlight.Pending)
	}

	updated, cmd := model.Update(session.AgentMessage{})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("expected pending entry to be cleared after flush")
	}

	// Verify that a Println command was returned
	if cmd == nil {
		t.Fatalf("expected tea.Println command after finalizing message")
	}
}

func TestToolEntryFlushesToTranscript(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(session.ToolCallStarted{
		ToolName: "bash",
		Args:     "ls",
	})
	model = updated.(Model)

	if model.InFlight.Pending == nil || model.InFlight.Pending.Role != session.Tool {
		t.Fatalf("expected pending tool entry")
	}

	updated, cmd := model.Update(session.ToolResult{
		ToolName: "bash",
		Result:   "ok",
	})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("expected pending entry to be cleared")
	}
	if cmd == nil {
		t.Fatalf("expected tea.Println command for tool result")
	}
}

func TestLayoutClampsComposerHeight(t *testing.T) {
	model := readyModel(t)

	// Initial height should be min (1)
	model.layout()
	if got := model.Input.Composer.Height(); got != minComposerHeight {
		t.Fatalf("expected initial composer height %d, got %d", minComposerHeight, got)
	}

	// 5 lines of text
	model.Input.Composer.SetValue("1\n2\n3\n4\n5")
	model.layout()

	// Should be 5
	if got := model.Input.Composer.Height(); got != 5 {
		t.Fatalf("expected composer height 5 for 5 lines, got %d", got)
	}

	// Over the max (10)
	model.Input.Composer.SetValue(strings.Repeat("line\n", 20))
	model.layout()

	if got := model.Input.Composer.Height(); got != maxComposerHeight {
		t.Fatalf("expected composer height to clamp to %d, got %d", maxComposerHeight, got)
	}
}

func TestProgressLineFitsWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 28
	model.Progress.Mode = stateError
	model.Progress.LastError = strings.Repeat("connection refused while reconnecting to the backend ", 3)

	if got := lipgloss.Width(model.progressLine()); got > model.App.Width {
		t.Fatalf(
			"expected progress line width <= %d, got %d: %q",
			model.App.Width,
			got,
			model.progressLine(),
		)
	}
}

func TestTurnFinishedLeavesProgressComplete(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.InFlight.Thinking = true
	model.Progress.TurnStartedAt = time.Now().Add(-3 * time.Second)
	model.Progress.CurrentTurnInput = 1200
	model.Progress.CurrentTurnOutput = 300

	updated, _ := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress = %v, want stateComplete", model.Progress.Mode)
	}
	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "✓ Complete") {
		t.Fatalf("progress line = %q, want complete state", line)
	}
	for _, want := range []string{"↑ 1.2k", "↓ 300", "3s"} {
		if !strings.Contains(line, want) {
			t.Fatalf("progress line = %q, missing %q", line, want)
		}
	}
	if strings.Index(line, "3s") < strings.Index(line, "↓ 300") {
		t.Fatalf("progress line = %q, want elapsed time after token counters", line)
	}
}

func TestErrorProgressLineUsesRedXSymbolCopy(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "backend failed"

	if got := ansi.Strip(model.progressLine()); !strings.Contains(got, "× Error: backend failed") {
		t.Fatalf("progress line = %q, want red x error copy", got)
	}
}

func TestRunningProgressLinePutsElapsedAfterTokenCounters(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.Progress.TurnStartedAt = time.Now().Add(-2 * time.Second)
	model.Progress.CurrentTurnInput = 3000
	model.Progress.CurrentTurnOutput = 84

	line := ansi.Strip(model.progressLine())
	for _, want := range []string{"Streaming...", "↑ 3.0k", "↓ 84", "2s", "Esc to cancel"} {
		if !strings.Contains(line, want) {
			t.Fatalf("progress line = %q, missing %q", line, want)
		}
	}
	if strings.Index(line, "2s") < strings.Index(line, "↓ 84") {
		t.Fatalf("progress line = %q, want elapsed time after token counters", line)
	}
}

func TestRunningProgressLineUsesCyanSpinner(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming

	line := model.progressLine()
	want := model.st.cyan.Render(model.Input.Spinner.Spinner.Frames[0])
	if !strings.Contains(line, want) {
		t.Fatalf("progress line = %q, want cyan spinner %q", line, want)
	}
}

func TestStatusLineFitsWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 32, Height: 24})
	model = updated.(Model)
	model.Model.Backend = stubBackend{
		sess:         &stubSession{events: make(chan session.Event)},
		provider:     "subscription-provider-with-a-very-long-name",
		model:        "model-name-that-would-wrap-in-a-small-terminal",
		contextLimit: 128000,
	}
	model.Progress.TokensSent = 45123
	model.Progress.TokensReceived = 78210
	model.Progress.TotalCost = 0.042
	model.App.Workdir = "/Users/nick/github/nijaru/ion"
	model.App.Branch = "feature/resize-persistence"

	if got := lipgloss.Width(model.statusLine()); got > model.App.Width {
		t.Fatalf(
			"expected status line width <= %d, got %d: %q",
			model.App.Width,
			got,
			model.statusLine(),
		)
	}
}

func TestStatusLineHidesZeroUsageBeforeFirstTurn(t *testing.T) {
	model := readyModel(t)
	model.Progress.TokensSent = 0
	model.Progress.TokensReceived = 0
	model.Progress.TotalCost = 0
	model.Model.Backend = stubBackend{sess: &stubSession{events: make(chan session.Event)}}

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "0 tokens") {
		t.Fatalf("status line should hide zero usage, got %q", line)
	}
	if strings.Contains(line, "k/") {
		t.Fatalf("status line should not show context usage without turns, got %q", line)
	}
}

func TestStatusLineIncludesThinkingLevel(t *testing.T) {
	model := readyModel(t)
	model.Progress.ReasoningEffort = "high"
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "o3-mini",
	}

	line := ansi.Strip(model.statusLine())
	if !strings.Contains(line, "high") {
		t.Fatalf("status line missing thinking level: %q", line)
	}
	if strings.Contains(line, "think=") {
		t.Fatalf("status line should not show the thinking key: %q", line)
	}
}

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
	model := readyModel(t)
	model.Model.Session = sess
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = updated.(Model)
	if cmd != nil {
		t.Fatal("esc while running should not print or quit")
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

func TestProviderItemsSortSetAPIsThenLocalThenUnset(t *testing.T) {
	for _, name := range []string{
		"ANTHROPIC_API_KEY",
		"OPENAI_API_KEY",
		"OPENROUTER_API_KEY",
		"GEMINI_API_KEY",
		"GOOGLE_API_KEY",
		"HF_TOKEN",
		"TOGETHER_API_KEY",
		"DEEPSEEK_API_KEY",
		"GROQ_API_KEY",
		"FIREWORKS_API_KEY",
		"MISTRAL_API_KEY",
		"MOONSHOT_API_KEY",
		"CEREBRAS_API_KEY",
		"ZAI_API_KEY",
		"XAI_API_KEY",
		"OPENAI_COMPATIBLE_API_KEY",
	} {
		t.Setenv(name, "")
	}
	t.Setenv("OPENROUTER_API_KEY", "test")
	t.Setenv("GOOGLE_API_KEY", "test")
	items := providerItems(&config.Config{})
	got := make([]string, 0, len(items))
	for _, item := range items {
		got = append(got, item.Label)
	}
	want := []string{
		"Gemini",
		"OpenRouter",
		"Ollama",
		"Local API",
		"Anthropic",
		"Cerebras",
		"DeepSeek",
		"Fireworks AI",
		"Groq",
		"Mistral",
		"Moonshot AI",
		"OpenAI",
		"Z.ai",
		"xAI",
		"Hugging Face",
		"Together AI",
	}
	if !slices.Equal(got, want) {
		t.Fatalf("provider order = %#v, want %#v", got, want)
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
		"provider = \"openai\"\nmodel = \"gpt-4.1\"\nreasoning_effort = \"auto\"\n",
	), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{sess: oldSession, provider: "openai", model: "gpt-4.1"}

	oldListModelsForConfig := registry.ListModelsForConfigHook
	registry.ListModelsForConfigHook = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "openai" {
			t.Fatalf("provider = %q, want openai", cfg.Provider)
		}
		return []registry.ModelMetadata{
			{ID: "gpt-4.1"},
			{ID: "gpt-4.1-mini"},
		}, nil
	}
	t.Cleanup(func() { registry.ListModelsForConfigHook = oldListModelsForConfig })

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
			newStorage := &stubStorageSession{id: sessionID, model: cfg.Provider + "/" + cfg.Model, branch: "main"}
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
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("primary model = %q, want gpt-4.1", got)
	}
	if !slices.Equal(observedModels, []string{"gpt-4.1-mini", "gpt-4.1"}) {
		t.Fatalf("switched models = %#v, want fast then primary", observedModels)
	}
}

func TestHandleCommandUpdatesConfigDirectly(t *testing.T) {
	tests := []struct {
		name        string
		command     string
		expected    string
		wantPicker  bool
		wantCommand bool
	}{
		{
			name:       "provider",
			command:    "/provider anthropic",
			expected:   "provider = 'anthropic'\nsession_retention_days = 90\n",
			wantPicker: true,
		},
		{
			name:        "model",
			command:     "/model gpt-4.1",
			expected:    "model = 'gpt-4.1'\nsession_retention_days = 90\n",
			wantCommand: true,
		},
		{
			name:        "thinking",
			command:     "/thinking high",
			expected:    "reasoning_effort = 'high'\nsession_retention_days = 90\n",
			wantCommand: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			home := t.TempDir()
			t.Setenv("HOME", home)

			oldListModelsForConfig := listModelsForConfig
			if tc.name == "provider" {
				listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
					return []registry.ModelMetadata{{ID: "anthropic-model"}}, nil
				}
			}
			t.Cleanup(func() { listModelsForConfig = oldListModelsForConfig })

			oldSession := &stubSession{events: make(chan session.Event)}
			oldBackend := stubBackend{sess: oldSession}
			model := New(oldBackend, nil, nil, "/tmp/test", "main", "dev", nil)

			model, cmd := model.handleCommand(tc.command)
			if tc.wantCommand && cmd == nil {
				t.Fatal("expected direct config command to return a cmd")
			}
			if !tc.wantCommand && cmd != nil {
				t.Fatalf("expected no cmd, got %T", cmd)
			}
			if tc.wantPicker && model.Picker.Overlay == nil {
				t.Fatal("expected picker to open")
			}
			if !tc.wantPicker && model.Picker.Overlay != nil {
				t.Fatal("expected no picker to open")
			}

			data, err := os.ReadFile(filepath.Join(home, ".ion", "config.toml"))
			if err != nil {
				t.Fatalf("read config: %v", err)
			}
			if got := string(data); got != tc.expected {
				t.Fatalf("config = %q, want %q", got, tc.expected)
			}
			if model.Progress.Status == "" {
				t.Fatal("expected status to be updated after direct config command")
			}
		})
	}
}

func TestCompactCommandUsesBackendCompactor(t *testing.T) {
	backend := &compactBackend{
		stubBackend: stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		compacted:   true,
	}
	model := New(backend, nil, nil, "/tmp/test", "main", "dev", nil)

	_, cmd := model.handleCommand("/compact")
	if cmd == nil {
		t.Fatal("expected /compact command to return a cmd")
	}

	msg := cmd()
	compacted, ok := msg.(sessionCompactedMsg)
	if !ok {
		t.Fatalf("expected sessionCompactedMsg, got %T", msg)
	}
	if !backend.called {
		t.Fatal("expected backend compactor to be called")
	}
	if compacted.notice != "Compacted current session context" {
		t.Fatalf("compact notice = %q", compacted.notice)
	}
}

func TestCompactCommandReportsNoOp(t *testing.T) {
	backend := &compactBackend{
		stubBackend: stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		compacted:   false,
	}
	model := New(backend, nil, nil, "/tmp/test", "main", "dev", nil)

	_, cmd := model.handleCommand("/compact")
	msg := cmd()
	compacted, ok := msg.(sessionCompactedMsg)
	if !ok {
		t.Fatalf("expected sessionCompactedMsg, got %T", msg)
	}
	if compacted.notice != "Session is already within compaction limits" {
		t.Fatalf("compact no-op notice = %q", compacted.notice)
	}
}

func TestCompactCommandErrorsWhenBackendUnsupported(t *testing.T) {
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	_, cmd := model.handleCommand("/compact")
	msg := cmd()
	errMsg, ok := msg.(session.Error)
	if !ok {
		t.Fatalf("expected session.Error, got %T", msg)
	}
	if errMsg.Err == nil || errMsg.Err.Error() != "current backend does not support /compact" {
		t.Fatalf("unexpected /compact error: %v", errMsg.Err)
	}
}

func TestClearCommandStartsFreshSession(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openai\"\nmodel = \"gpt-4.1\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{sess: oldSession, provider: "openai", model: "gpt-4.1"}

	var observedSessionID string
	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			observedSessionID = sessionID
			newStorage := &stubStorageSession{
				id:     "fresh-session",
				model:  cfg.Provider + "/" + cfg.Model,
				branch: "main",
			}
			newBackend := testutil.New()
			newBackend.SetConfig(cfg)
			newBackend.SetSession(newStorage)
			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	model, cmd := model.handleCommand("/clear")
	if cmd == nil {
		t.Fatal("expected /clear command to return a cmd")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	if observedSessionID != "" {
		t.Fatalf(
			"session ID passed to clear switcher = %q, want empty for fresh session",
			observedSessionID,
		)
	}
	if switched.notice != "Started fresh session" {
		t.Fatalf("clear notice = %q", switched.notice)
	}
}

func TestClearCommandFallsBackToActiveRuntimeConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("session_retention_days = 90\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{
		sess:     oldSession,
		provider: "openrouter",
		model:    "deepseek/deepseek-v3.2",
	}

	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			if cfg.Provider != "openrouter" {
				t.Fatalf("provider = %q, want openrouter", cfg.Provider)
			}
			if cfg.Model != "deepseek/deepseek-v3.2" {
				t.Fatalf("model = %q, want deepseek/deepseek-v3.2", cfg.Model)
			}
			newStorage := &stubStorageSession{id: "fresh-session"}
			newBackend := testutil.New()
			newBackend.SetConfig(cfg)
			newBackend.SetSession(newStorage)
			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	_, cmd := model.handleCommand("/clear")
	msg := cmd()
	if _, ok := msg.(runtimeSwitchedMsg); !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
}

func TestCostCommandReportsSessionTotals(t *testing.T) {
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		&stubStorageSession{usageIn: 1200, usageOut: 300, usageCost: 0.012345},
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	_, cmd := model.handleCommand("/cost")
	msg := cmd()
	costMsg, ok := msg.(sessionCostMsg)
	if !ok {
		t.Fatalf("expected sessionCostMsg, got %T", msg)
	}
	for _, want := range []string{"input tokens: 1200", "output tokens: 300", "total tokens: 1500", "cost: $0.012345"} {
		if !strings.Contains(costMsg.notice, want) {
			t.Fatalf("cost notice missing %q: %q", want, costMsg.notice)
		}
	}
}

func TestCostCommandReportsMissingCost(t *testing.T) {
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		&stubStorageSession{},
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	_, cmd := model.handleCommand("/cost")
	msg := cmd()
	costMsg, ok := msg.(sessionCostMsg)
	if !ok {
		t.Fatalf("expected sessionCostMsg, got %T", msg)
	}
	if costMsg.notice != "No API cost tracked for this session" {
		t.Fatalf("cost notice = %q", costMsg.notice)
	}
}

func TestHelpCommandReportsCurrentCommandsAndKeys(t *testing.T) {
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	_, cmd := model.handleCommand("/help")
	msg := cmd()
	helpMsg, ok := msg.(sessionHelpMsg)
	if !ok {
		t.Fatalf("expected sessionHelpMsg, got %T", msg)
	}

	for _, want := range []string{
		"/resume [id]",
		"/primary",
		"/fast",
		"/provider [name]",
		"/model [name]",
		"/thinking [lvl]",
		"/compact",
		"/clear",
		"/cost",
		"/mcp add <cmd>",
		"/quit, /exit",
		"/help",
		"Ctrl+P",
		"Tab",
		"Shift+Tab",
		"Esc",
		"Up / Down",
		"Enter",
		"Ctrl+C",
	} {
		if !strings.Contains(helpMsg.notice, want) {
			t.Fatalf("help notice missing %q: %q", want, helpMsg.notice)
		}
	}
	if strings.Contains(helpMsg.notice, "/tree") {
		t.Fatalf("help notice should not advertise /tree yet: %q", helpMsg.notice)
	}
}

func TestProviderItemsShowConfiguredStatus(t *testing.T) {
	t.Setenv("OPENROUTER_API_KEY", "test-key")
	items := providerItems(&config.Config{})

	for label, wantDetail := range map[string]string{
		"Anthropic":  "Set ANTHROPIC_API_KEY",
		"OpenRouter": "Ready",
		"Ollama":     "Ready",
	} {
		found := false
		for _, item := range items {
			if item.Label != label {
				continue
			}
			found = true
			if item.Detail != wantDetail {
				t.Fatalf("provider %q detail = %q, want %q", item.Label, item.Detail, wantDetail)
			}
		}
		if !found {
			t.Fatalf("provider %q not found", label)
		}
	}
}

func TestModelItemsUseInjectedModelLister(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", cfg.Provider)
		}
		return []registry.ModelMetadata{
			{
				ID:               "z-ai/glm-4.5",
				Created:          time.Date(2025, 2, 1, 0, 0, 0, 0, time.UTC).Unix(),
				ContextLimit:     64000,
				InputPrice:       1.23,
				OutputPrice:      4.56,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{
				ID:               "openai/gpt-4.1",
				Created:          time.Date(2025, 4, 1, 0, 0, 0, 0, time.UTC).Unix(),
				ContextLimit:     128000,
				InputPrice:       0.1,
				OutputPrice:      0.2,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{
				ID:               "z-ai/glm-5",
				Created:          time.Date(2025, 5, 1, 0, 0, 0, 0, time.UTC).Unix(),
				ContextLimit:     128000,
				InputPrice:       0.2,
				OutputPrice:      0.4,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	items, err := modelItemsForProvider(&config.Config{Provider: "openrouter"})
	if err != nil {
		t.Fatalf("modelItemsForProvider: %v", err)
	}
	if len(items) != 3 {
		t.Fatalf("items len = %d, want 3", len(items))
	}
	wantOrder := []string{"openai/gpt-4.1", "z-ai/glm-5", "z-ai/glm-4.5"}
	gotOrder := []string{items[0].Label, items[1].Label, items[2].Label}
	if !slices.Equal(gotOrder, wantOrder) {
		t.Fatalf("items not sorted by org/newest: got %#v want %#v", gotOrder, wantOrder)
	}
	if items[0].Metrics == nil {
		t.Fatal("expected model metrics")
	}
	if items[0].Metrics.Context != "128k" || items[0].Metrics.Input != "$0.10" ||
		items[0].Metrics.Output != "$0.20" {
		t.Fatalf("unexpected model metrics: %#v", items[0].Metrics)
	}
}

func TestModelItemsTreatZeroPricesAsFreeSearchTerm(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		return []registry.ModelMetadata{
			{
				ID:               "vendor/model-free",
				ContextLimit:     128000,
				InputPrice:       0,
				OutputPrice:      0,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{
				ID:               "vendor/model-paid",
				ContextLimit:     128000,
				InputPrice:       0.1,
				OutputPrice:      0.2,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{ID: "vendor/model-unknown", ContextLimit: 128000},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	items, err := modelItemsForProvider(&config.Config{Provider: "openrouter"})
	if err != nil {
		t.Fatalf("modelItemsForProvider: %v", err)
	}

	filtered := rankedPickerItems(items, "free")
	got := make([]string, 0, len(filtered))
	for _, item := range filtered {
		got = append(got, item.Label)
	}
	if !slices.Contains(got, "vendor/model-free") {
		t.Fatalf("expected zero-priced model to match free query, got %v", got)
	}
	if slices.Contains(got, "vendor/model-paid") {
		t.Fatalf("did not expect paid model to match free query, got %v", got)
	}
	if slices.Contains(got, "vendor/model-unknown") {
		t.Fatalf("did not expect unknown-priced model to match free query, got %v", got)
	}
}

func TestModelMetricsRenderFreeAndUnknownDistinctly(t *testing.T) {
	free := modelMetrics(registry.ModelMetadata{
		ContextLimit:     128000,
		InputPrice:       0,
		OutputPrice:      0,
		InputPriceKnown:  true,
		OutputPriceKnown: true,
	})
	if free == nil || free.Input != "Free" || free.Output != "Free" {
		t.Fatalf("expected free metrics, got %#v", free)
	}

	unknown := modelMetrics(registry.ModelMetadata{
		ContextLimit: 128000,
	})
	if unknown == nil {
		t.Fatal("expected context-only metrics")
	}
	if unknown.Input != "" || unknown.Output != "" {
		t.Fatalf("expected unknown pricing to stay blank, got %#v", unknown)
	}
}

func TestPickerFilteringMatchesTypedQuery(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a provider",
		items: []pickerItem{
			{Label: "Anthropic", Value: "anthropic", Detail: "Set ANTHROPIC_API_KEY"},
			{Label: "OpenRouter", Value: "openrouter", Detail: "Ready"},
		},
		filtered: []pickerItem{
			{Label: "Anthropic", Value: "anthropic", Detail: "Set ANTHROPIC_API_KEY"},
			{Label: "OpenRouter", Value: "openrouter", Detail: "Ready"},
		},
		purpose: pickerPurposeProvider,
	}

	for _, r := range []rune("router") {
		model, _ = model.handlePickerKey(tea.KeyPressMsg{Text: string(r), Code: r})
	}

	if got := len(pickerDisplayItems(model.Picker.Overlay)); got != 1 {
		t.Fatalf("filtered items = %d, want 1", got)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[0].Label; got != "OpenRouter" {
		t.Fatalf("filtered label = %q, want OpenRouter", got)
	}
}

func TestPickerFilteringRanksClosestMatchesFirst(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model for openrouter",
		items: []pickerItem{
			{Label: "z-ai/glm-5-turbo", Value: "z-ai/glm-5-turbo"},
			{Label: "z-ai/glm-5", Value: "z-ai/glm-5"},
			{Label: "z-ai/glm-4.5", Value: "z-ai/glm-4.5"},
		},
		filtered: []pickerItem{
			{Label: "z-ai/glm-5-turbo", Value: "z-ai/glm-5-turbo"},
			{Label: "z-ai/glm-5", Value: "z-ai/glm-5"},
			{Label: "z-ai/glm-4.5", Value: "z-ai/glm-4.5"},
		},
		purpose: pickerPurposeModel,
	}

	for _, r := range []rune("glm-5") {
		model, _ = model.handlePickerKey(tea.KeyPressMsg{Text: string(r), Code: r})
	}

	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 2 {
		t.Fatalf("filtered items = %d, want 2", len(items))
	}
	if items[0].Label != "z-ai/glm-5" {
		t.Fatalf("top match = %q, want z-ai/glm-5", items[0].Label)
	}
	if items[1].Label != "z-ai/glm-5-turbo" {
		t.Fatalf("second match = %q, want z-ai/glm-5-turbo", items[1].Label)
	}
	for _, item := range items {
		if item.Label == "z-ai/glm-4.5" {
			t.Fatalf("unexpected loose match for glm-5 query: %+v", items)
		}
	}
}

func TestModelPickerRendersSeparatePriceColumns(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model for openrouter",
		items: []pickerItem{
			{
				Label: "z-ai/glm-5",
				Value: "z-ai/glm-5",
				Metrics: &pickerMetrics{
					Context: "80k",
					Input:   "$0.72",
					Output:  "$2.30",
				},
			},
			{
				Label: "z-ai/glm-5-turbo",
				Value: "z-ai/glm-5-turbo",
				Metrics: &pickerMetrics{
					Context: "202k",
					Input:   "$1.20",
					Output:  "$4.00",
				},
			},
		},
		filtered: []pickerItem{
			{
				Label: "z-ai/glm-5",
				Value: "z-ai/glm-5",
				Metrics: &pickerMetrics{
					Context: "80k",
					Input:   "$0.72",
					Output:  "$2.30",
				},
			},
			{
				Label: "z-ai/glm-5-turbo",
				Value: "z-ai/glm-5-turbo",
				Metrics: &pickerMetrics{
					Context: "202k",
					Input:   "$1.20",
					Output:  "$4.00",
				},
			},
		},
		purpose: pickerPurposeModel,
	}

	rendered := ansi.Strip(model.renderPicker())
	if !strings.Contains(rendered, "Model") || !strings.Contains(rendered, "Context") ||
		!strings.Contains(rendered, "Input") ||
		!strings.Contains(rendered, "Output") {
		t.Fatalf("rendered picker missing header row: %q", rendered)
	}
	var header, rowA, rowB string
	for _, line := range strings.Split(rendered, "\n") {
		switch {
		case strings.Contains(line, "Model") && strings.Contains(line, "Context") && strings.Contains(line, "Input") && strings.Contains(line, "Output"):
			header = line
		case strings.Contains(line, "z-ai/glm-5-turbo"):
			rowA = line
		case strings.Contains(line, "z-ai/glm-5") && !strings.Contains(line, "turbo"):
			rowB = line
		}
	}
	if header == "" || rowA == "" || rowB == "" {
		t.Fatalf("did not find model rows in rendered picker: %q", rendered)
	}
	if !strings.Contains(rowA, "202k") || !strings.Contains(rowB, "80k") ||
		!strings.Contains(rowA, "$1.20") || !strings.Contains(rowB, "$0.72") ||
		!strings.Contains(rowA, "$4.00") || !strings.Contains(rowB, "$2.30") {
		t.Fatalf("missing detail columns in rendered picker: %q", rendered)
	}
	headerContext := lipgloss.Width(header[:strings.Index(header, "Context")])
	rowAContext := lipgloss.Width(rowA[:strings.Index(rowA, "202k")])
	rowBContext := lipgloss.Width(rowB[:strings.Index(rowB, "80k")])
	if headerContext != rowAContext || headerContext != rowBContext {
		t.Fatalf("context column not aligned:\nheader=%q\nrowA=%q\nrowB=%q", header, rowA, rowB)
	}
	headerInput := lipgloss.Width(header[:strings.Index(header, "Input")])
	rowAInput := lipgloss.Width(rowA[:strings.Index(rowA, "$1.20")])
	rowBInput := lipgloss.Width(rowB[:strings.Index(rowB, "$0.72")])
	if headerInput != rowAInput || headerInput != rowBInput {
		t.Fatalf("input column not aligned:\nheader=%q\nrowA=%q\nrowB=%q", header, rowA, rowB)
	}
	headerOutput := lipgloss.Width(header[:strings.Index(header, "Output")])
	rowAOutput := lipgloss.Width(rowA[:strings.Index(rowA, "$4.00")])
	rowBOutput := lipgloss.Width(rowB[:strings.Index(rowB, "$2.30")])
	if headerOutput != rowAOutput || headerOutput != rowBOutput {
		t.Fatalf("output column not aligned:\nheader=%q\nrowA=%q\nrowB=%q", header, rowA, rowB)
	}
}

func TestPickerFilteringAcceptsSpaceInput(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a provider",
		items: []pickerItem{
			{Label: "alpha", Value: "alpha", Detail: "Set ALPHA_API_KEY"},
			{Label: "beta", Value: "beta", Detail: "Ready"},
		},
		filtered: []pickerItem{
			{Label: "alpha", Value: "alpha", Detail: "Set ALPHA_API_KEY"},
			{Label: "beta", Value: "beta", Detail: "Ready"},
		},
		purpose: pickerPurposeProvider,
	}

	for _, key := range []tea.KeyPressMsg{
		{Text: "s", Code: 's'},
		{Text: "e", Code: 'e'},
		{Text: "t", Code: 't'},
		{Text: " ", Code: tea.KeySpace},
		{Text: "A", Code: 'A'},
		{Text: "L", Code: 'L'},
		{Text: "P", Code: 'P'},
		{Text: "H", Code: 'H'},
		{Text: "A", Code: 'A'},
	} {
		model, _ = model.handlePickerKey(key)
	}

	if got := model.Picker.Overlay.query; got != "set ALPHA" {
		t.Fatalf("picker query = %q, want %q", got, "set ALPHA")
	}
	if got := len(pickerDisplayItems(model.Picker.Overlay)); got != 1 {
		t.Fatalf("filtered items = %d, want 1", got)
	}
}

func TestModelPickerListsFavoritesAtTop(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", cfg.Provider)
		}
		return []registry.ModelMetadata{
			{ID: "vendor/model-a"},
			{ID: "vendor/model-b"},
			{ID: "vendor/model-c"},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider:  "openrouter",
		Model:     "vendor/model-b",
		FastModel: "vendor/model-a",
	})
	model = updated
	if cmd != nil {
		t.Fatalf("openModelPickerWithConfig returned unexpected command %T", cmd)
	}
	if model.Picker.Overlay == nil {
		t.Fatal("expected model picker overlay")
	}
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 3 {
		t.Fatalf("item count = %d, want 3", len(items))
	}
	if items[0].Group != "Favorites" || items[1].Group != "Favorites" {
		t.Fatalf("favorites groups = [%q %q], want [Favorites Favorites]", items[0].Group, items[1].Group)
	}
	if items[0].Detail != "Primary" || items[1].Detail != "Fast" {
		t.Fatalf("favorites details = [%q %q], want [Primary Fast]", items[0].Detail, items[1].Detail)
	}
	if items[0].Value != "vendor/model-b" || items[1].Value != "vendor/model-a" {
		t.Fatalf("favorites values = [%q %q], want [vendor/model-b vendor/model-a]", items[0].Value, items[1].Value)
	}
	if items[2].Group != "All models" {
		t.Fatalf("catalog group = %q, want All models", items[2].Group)
	}

	rendered := ansi.Strip(model.renderPicker())
	if !strings.Contains(rendered, "Favorites") || !strings.Contains(rendered, "All models") {
		t.Fatalf("rendered picker missing model groups: %q", rendered)
	}
	if !strings.Contains(rendered, "Primary") || !strings.Contains(rendered, "Fast") {
		t.Fatalf("rendered picker missing favorite group labels: %q", rendered)
	}
}

func TestModelPickerTabReturnsToProviderPicker(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model for openrouter",
		items: []pickerItem{
			{Label: "vendor/model-b", Value: "vendor/model-b", Group: "Favorites", Detail: "Primary"},
			{Label: "vendor/model-a", Value: "vendor/model-a", Group: "Favorites", Detail: "Fast"},
		},
		filtered: []pickerItem{
			{Label: "vendor/model-b", Value: "vendor/model-b", Group: "Favorites", Detail: "Primary"},
			{Label: "vendor/model-a", Value: "vendor/model-a", Group: "Favorites", Detail: "Fast"},
		},
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "openrouter"},
	}

	updated, _ := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated

	if model.Picker.Overlay == nil {
		t.Fatal("expected provider picker to open")
	}
	if model.Picker.Overlay.purpose != pickerPurposeProvider {
		t.Fatalf("picker purpose = %v, want provider picker", model.Picker.Overlay.purpose)
	}
}

func TestModelPickerPageKeysJumpByPage(t *testing.T) {
	model := readyModel(t)
	items := make([]pickerItem, 12)
	for i := range items {
		value := "model-" + string(rune('a'+i))
		items[i] = pickerItem{
			Label:  value,
			Value:  value,
			Group:  "All models",
			Search: pickerSearchIndex(value, value, "", "", nil),
		}
	}
	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a model",
		items:    items,
		filtered: slices.Clone(items),
		index:    0,
		purpose:  pickerPurposeModel,
		cfg:      &config.Config{Provider: "openrouter"},
	}

	updated, _ := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyPgDown})
	model = updated
	if got := model.Picker.Overlay.index; got != pickerPageSize {
		t.Fatalf("index after pgdown = %d, want %d", got, pickerPageSize)
	}

	updated, _ = model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyPgUp})
	model = updated
	if got := model.Picker.Overlay.index; got != 0 {
		t.Fatalf("index after pgup = %d, want 0", got)
	}
}

func TestQueuedFollowUpSubmitsAfterTurnFinished(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Input.Composer.SetValue("follow up")
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)
	if len(model.InFlight.QueuedTurns) != 1 || model.InFlight.QueuedTurns[0] != "follow up" {
		t.Fatalf("queuedTurns = %v, want [follow up]", model.InFlight.QueuedTurns)
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after queueing", got)
	}
	if cmd == nil {
		t.Fatal("expected queue notice cmd")
	}

	updated, cmd = model.Update(session.TurnFinished{})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected queued turn command after finish")
	}
	msg := cmd()
	next, nextCmd := model.Update(msg)
	model = next.(Model)
	if nextCmd == nil {
		t.Fatal("expected queued turn submission command")
	}
	if len(sess.submits) != 1 || sess.submits[0] != "follow up" {
		t.Fatalf("submits = %#v, want queued follow up", sess.submits)
	}
}

func TestPickerCommitSwitchesRuntime(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openai\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{sess: oldSession}

	switched := false
	observedSessionID := ""
	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			switched = true
			observedSessionID = sessionID

			resolved := *cfg
			resolved.Provider = "openai"

			newStorage := &stubStorageSession{
				id:     sessionID,
				model:  resolved.Model,
				branch: "feature/switch",
			}

			newBackend := testutil.New()
			newBackend.SetConfig(&resolved)
			newBackend.SetSession(newStorage)

			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	model.Picker.Overlay = &pickerOverlayState{
		title:   "Pick a model for openai",
		items:   []pickerItem{{Label: "gpt-4.1", Value: "gpt-4.1"}},
		index:   0,
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "openai"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated
	msg := cmd()

	switchedMsg, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}

	next, _ := model.Update(switchedMsg)
	model = next.(Model)

	if !switched {
		t.Fatal("expected runtime switch callback to be invoked")
	}
	if observedSessionID != oldSession.ID() {
		t.Fatalf("session ID passed to switcher = %q, want %q", observedSessionID, oldSession.ID())
	}
	if got := model.Model.Backend.Provider(); got != "openai" {
		t.Fatalf("backend provider = %q, want %q", got, "openai")
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model = %q, want %q", got, "gpt-4.1")
	}
	if got := model.Model.Session.ID(); got != oldSession.ID() {
		t.Fatalf("session ID = %q, want %q", got, oldSession.ID())
	}
	if got := model.Model.Storage.ID(); got != oldSession.ID() {
		t.Fatalf("storage session ID = %q, want %q", got, oldSession.ID())
	}
	if got := model.App.Branch; got != "feature/switch" {
		t.Fatalf("branch = %q, want %q", got, "feature/switch")
	}
}

func TestPickerCommitSameModelIsNoOp(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}
	model.Picker.Overlay = &pickerOverlayState{
		title:   "Pick a model for openrouter",
		items:   []pickerItem{{Label: "z-ai/glm-5", Value: "z-ai/glm-5"}},
		index:   0,
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "openrouter", Model: "z-ai/glm-5"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated

	if cmd != nil {
		t.Fatalf("expected no command when selecting the active model, got %T", cmd)
	}
	if model.Picker.Overlay != nil {
		t.Fatal("expected picker to close on same-model selection")
	}
	if got := model.Model.Backend.Model(); got != "z-ai/glm-5" {
		t.Fatalf("backend model = %q, want z-ai/glm-5", got)
	}
}

func TestProviderPickerSelectingCurrentProviderOpensModelPickerWithoutClearingModel(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}
	oldListModels := listModels
	listModels = func(ctx context.Context, provider string) ([]registry.ModelMetadata, error) {
		if provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", provider)
		}
		return []registry.ModelMetadata{
			{ID: "z-ai/glm-4.5"},
			{ID: "z-ai/glm-5"},
			{ID: "z-ai/glm-5-turbo"},
		}, nil
	}
	defer func() { listModels = oldListModels }()

	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    providerItems(&config.Config{}),
		filtered: providerItems(&config.Config{}),
		index:    pickerIndex(providerItems(&config.Config{}), "openrouter"),
		purpose:  pickerPurposeProvider,
		cfg:      &config.Config{Provider: "openrouter", Model: "z-ai/glm-5"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated
	if cmd != nil {
		t.Fatalf("expected no command when reopening model picker, got %T", cmd)
	}
	if model.Picker.Overlay == nil {
		t.Fatal("expected model picker to open")
	}
	if model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker purpose = %v, want model picker", model.Picker.Overlay.purpose)
	}
	if model.Picker.Overlay.cfg == nil {
		t.Fatal("expected picker config to be preserved")
	}
	if got := model.Picker.Overlay.cfg.Provider; got != "openrouter" {
		t.Fatalf("picker provider = %q, want openrouter", got)
	}
	if got := model.Picker.Overlay.cfg.Model; got != "z-ai/glm-5" {
		t.Fatalf("picker model = %q, want z-ai/glm-5", got)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[model.Picker.Overlay.index].Value; got != "z-ai/glm-5" {
		t.Fatalf("selected model = %q, want z-ai/glm-5", got)
	}
	if got := model.Model.Backend.Provider(); got != "openrouter" {
		t.Fatalf("backend provider = %q, want openrouter", got)
	}
	if got := model.Model.Backend.Model(); got != "z-ai/glm-5" {
		t.Fatalf("backend model = %q, want z-ai/glm-5", got)
	}
}

func TestRuntimeSwitchKeepsNoticesOutOfTranscriptStorage(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openai\"\nmodel = \"gpt-4.1\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{sess: oldSession}

	newStorage := &stubStorageSession{
		id:     oldSession.ID(),
		model:  "openai/gpt-4.1",
		branch: "feature/switch",
	}
	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			resolved := *cfg
			newBackend := testutil.New()
			newBackend.SetConfig(&resolved)
			newBackend.SetSession(newStorage)
			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	next, _ := model.Update(runtimeSwitchedMsg{
		backend: testutil.New(),
		session: testutil.New(),
		storage: newStorage,
		status:  "ready",
		notice:  "Switched model to gpt-4.1",
	})
	model = next.(Model)

	if len(newStorage.appends) != 0 {
		t.Fatalf(
			"expected runtime switch notice to stay out of transcript storage, got %d appends",
			len(newStorage.appends),
		)
	}
}

func TestRuntimeSwitchClearsQueuedTurns(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{"stale follow up"}
	model.Progress.LastError = "old error"
	model.Progress.LastTurnSummary = turnSummary{Elapsed: time.Second, Input: 1, Output: 2, Cost: 3}

	next, _ := model.Update(runtimeSwitchedMsg{
		backend: stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		session: &stubSession{events: make(chan session.Event)},
		storage: &stubStorageSession{id: "session-1", branch: "main"},
		status:  "ready",
	})
	model = next.(Model)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want cleared on runtime switch", model.InFlight.QueuedTurns)
	}
	if model.Progress.LastError != "" {
		t.Fatalf("last error = %q, want cleared on runtime switch", model.Progress.LastError)
	}
	if model.Progress.LastTurnSummary != (turnSummary{}) {
		t.Fatalf("last turn summary = %#v, want cleared on runtime switch", model.Progress.LastTurnSummary)
	}
}

func TestSessionErrorClearsQueuedTurnsAndSetsError(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{"stale follow up"}
	model.Progress.LastError = "old error"

	next, _ := model.Update(session.Error{Err: errors.New("backend failed")})
	model = next.(Model)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want cleared on session error", model.InFlight.QueuedTurns)
	}
	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "backend failed" {
		t.Fatalf("last error = %q, want backend failed", model.Progress.LastError)
	}
}

func TestSubmitTextPropagatesImmediateSessionError(t *testing.T) {
	sess := &stubSession{
		events:    make(chan session.Event),
		submitErr: errors.New("backend unavailable"),
	}
	storeSess := &stubStorageSession{id: "stub-session"}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storeSess
	model.Input.Composer.SetValue("hello")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "backend unavailable" {
		t.Fatalf("last error = %q, want backend unavailable", model.Progress.LastError)
	}
	if !model.Progress.TurnStartedAt.IsZero() {
		t.Fatalf("turn started at = %v, want zero after immediate submit failure", model.Progress.TurnStartedAt)
	}
	if len(sess.submits) != 0 {
		t.Fatalf("submit count = %d, want 0 after immediate failure", len(sess.submits))
	}
	if cmd == nil {
		t.Fatal("expected follow-up command to render transcript entries")
	}
	if len(storeSess.appends) != 2 {
		t.Fatalf("storage appends = %d, want user + system error entries", len(storeSess.appends))
	}
	if got := storeSess.appends[1]; got == nil {
		t.Fatal("expected persisted system error entry")
	} else if sys, ok := got.(storage.System); !ok {
		t.Fatalf("persisted error entry type = %T, want storage.System", got)
	} else if sys.Content != "Error: backend unavailable" {
		t.Fatalf("persisted error content = %q, want wrapped backend error", sys.Content)
	}
}

func TestSlashModelSameValueIsNoOp(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openrouter\"\nmodel = \"z-ai/glm-5\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}

	model, cmd := model.handleCommand("/model z-ai/glm-5")
	if cmd != nil {
		t.Fatalf("expected no-op command for same model, got %T", cmd)
	}
}

func TestRuntimeSwitchShowsStatusOnResume(t *testing.T) {
	model := readyModel(t)
	model.Model.Session = &stubSession{events: make(chan session.Event)}

	updated, cmd := model.Update(runtimeSwitchedMsg{
		backend:       stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		session:       &stubSession{events: make(chan session.Event)},
		storage:       &stubStorageSession{id: "session-1", branch: "main"},
		printLines:    []string{"--- resumed ---", "ion v0.0.0", "~/tmp/test • main"},
		replayEntries: []session.Entry{{Role: session.User, Content: "hello"}},
		status:        "Connected via Canto",
		notice:        "Resumed session session-1",
		showStatus:    false,
	})
	model = updated.(Model)

	if model.Progress.Status != "Connected via Canto" {
		t.Fatalf("status = %q", model.Progress.Status)
	}
	if cmd == nil {
		t.Fatal("expected command batch for runtime switch")
	}
}

func TestResumeStoredSessionClosesInspectionSession(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openai\"\nmodel = \"gpt-4.1\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	tempSession := &stubStorageSession{
		id:     "session-1",
		model:  "openai/gpt-4.1",
		branch: "main",
	}

	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		nil,
		&resumeOnlyStore{resumed: tempSession},
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			newBackend := testutil.New()
			opened := &stubStorageSession{
				id:     sessionID,
				model:  cfg.Provider + "/" + cfg.Model,
				branch: "feature/resume",
			}
			newBackend.SetConfig(cfg)
			newBackend.SetSession(opened)
			return newBackend, newBackend.Session(), opened, nil
		},
	)

	cmd := model.resumeStoredSessionByID("session-1")
	msg := cmd()

	if _, ok := msg.(runtimeSwitchedMsg); !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	if !tempSession.closed {
		t.Fatal("expected temporary inspection session to be closed after reading metadata")
	}
}

func TestStartupPrintLinesIncludesReplayHistory(t *testing.T) {
	model := readyModel(t)
	model.App.StartupLines = []string{"line-1", "line-2"}
	model.Progress.Status = "ready"
	model.App.StartupEntries = []session.Entry{
		{Role: session.User, Content: "hello"},
		{Role: session.Agent, Content: "world"},
	}

	lines := model.startupPrintLines()
	want := []string{
		"line-1",
		"line-2",
		model.headerLine(),
		"",
		model.renderStartupStatus("ready"),
		model.renderEntry(session.Entry{Role: session.User, Content: "hello"}),
		model.renderEntry(session.Entry{Role: session.Agent, Content: "world"}),
	}

	if len(lines) != len(want) {
		t.Fatalf("startup lines length = %d, want %d", len(lines), len(want))
	}
	for i := range want {
		if lines[i] != want[i] {
			t.Fatalf("startup line %d = %q, want %q", i, lines[i], want[i])
		}
	}
}

func TestStartupPrintLinesOmitsConfigurationWarning(t *testing.T) {
	model := readyModel(t)
	model.Progress.Status = noProviderConfiguredStatus()

	lines := model.startupPrintLines()
	for _, line := range lines {
		if strings.Contains(line, "No provider configured") {
			t.Fatalf("startup lines should not include config warning: %#v", lines)
		}
	}
}

func TestProgressLineShowsConfigurationWarning(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "",
		modelSet:    true,
	}

	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "No model configured") {
		t.Fatalf("progress line missing config warning: %q", line)
	}
}

func TestProgressLineIgnoresStaleConfigurationStatusWhenBackendIsConfigured(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "z-ai/glm-5",
		modelSet:    true,
	}
	model.Progress.Status = noModelConfiguredStatus()

	line := ansi.Strip(model.progressLine())
	if strings.Contains(line, "No model configured") {
		t.Fatalf(
			"progress line should ignore stale config warning when backend is configured: %q",
			line,
		)
	}
	if !strings.Contains(line, "Ready") {
		t.Fatalf("progress line = %q, want Ready", line)
	}
}

func TestProgressLineShowsMeaningfulRestoredStatus(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "z-ai/glm-5",
		modelSet:    true,
	}
	model.Progress.Status = "Running tests"

	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "Running tests") {
		t.Fatalf("progress line missing restored status: %q", line)
	}
}

func TestProgressLineHidesBootstrapConnectedStatus(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "z-ai/glm-5",
		modelSet:    true,
	}
	model.Progress.Status = "Connected via Canto"

	line := ansi.Strip(model.progressLine())
	if strings.Contains(line, "Connected via Canto") {
		t.Fatalf("progress line should suppress bootstrap connection notice: %q", line)
	}
	if !strings.Contains(line, "Ready") {
		t.Fatalf("progress line = %q, want Ready", line)
	}
}

func TestProviderItemsUseCatalogGroups(t *testing.T) {
	items := providerItems(&config.Config{})
	if len(items) < 9 {
		t.Fatalf("provider items = %d, want broad catalog", len(items))
	}
	for _, item := range items {
		if item.Group == "" {
			t.Fatalf("provider %q should have a picker group", item.Label)
		}
	}
}

func TestProviderItemsPreferReadyProvidersBeforeUnsetOnes(t *testing.T) {
	for _, name := range []string{
		"ANTHROPIC_API_KEY",
		"OPENAI_API_KEY",
		"OPENROUTER_API_KEY",
		"GEMINI_API_KEY",
		"GOOGLE_API_KEY",
		"HF_TOKEN",
		"TOGETHER_API_KEY",
		"DEEPSEEK_API_KEY",
		"GROQ_API_KEY",
		"FIREWORKS_API_KEY",
		"MISTRAL_API_KEY",
		"MOONSHOT_API_KEY",
		"CEREBRAS_API_KEY",
		"ZAI_API_KEY",
		"XAI_API_KEY",
		"OPENAI_COMPATIBLE_API_KEY",
	} {
		t.Setenv(name, "")
	}
	t.Setenv("OPENROUTER_API_KEY", "test")
	t.Setenv("GOOGLE_API_KEY", "test")

	items := providerItems(&config.Config{})
	indexOf := func(value string) int {
		for i, item := range items {
			if item.Value == value {
				return i
			}
		}
		return -1
	}

	if indexOf("gemini") == -1 || indexOf("openrouter") == -1 || indexOf("local-api") == -1 {
		t.Fatalf("expected ready providers and Local API to appear in picker: %#v", items)
	}
	if indexOf("anthropic") == -1 {
		t.Fatalf("expected anthropic in picker")
	}
	if indexOf("gemini") > indexOf("anthropic") || indexOf("openrouter") > indexOf("anthropic") {
		t.Fatalf("ready remote providers should sort before unset direct providers")
	}
	if indexOf("local-api") > indexOf("anthropic") {
		t.Fatalf("Local API should sort ahead of unset direct providers")
	}
}

func TestProviderItemsHideCustomEndpointByDefault(t *testing.T) {
	items := providerItems(&config.Config{})
	for _, item := range items {
		if item.Value == "openai-compatible" {
			t.Fatalf("custom endpoint entry %q should be hidden by default", item.Value)
		}
	}
	foundLocal := false
	for _, item := range items {
		if item.Value == "local-api" && item.Label == "Local API" {
			foundLocal = true
			break
		}
	}
	if !foundLocal {
		t.Fatalf("Local API should always be visible")
	}

	items = providerItems(
		&config.Config{Provider: "openai-compatible", Endpoint: "https://example.com/v1"},
	)
	found := false
	for _, item := range items {
		if item.Value == "openai-compatible" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("custom endpoint entry should be shown when configured")
	}

	items = providerItems(&config.Config{Provider: "local-api", Endpoint: "http://127.0.0.1:1/v1"})
	found = false
	for _, item := range items {
		if item.Value == "local-api" && item.Label == "Local API" {
			if item.Detail != "Not running" {
				t.Fatalf("local-api detail = %q, want %q", item.Detail, "Not running")
			}
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("local-api should render when active")
	}
}
