package app

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/canto/memory"
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
	if b.provider != "" {
		return b.provider
	}
	return "stub"
}
func (b stubBackend) Model() string {
	if b.model != "" {
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
	events  chan session.Event
	submits []string
}

func (s *stubSession) Open(ctx context.Context) error              { return nil }
func (s *stubSession) Resume(ctx context.Context, id string) error { return nil }
func (s *stubSession) SubmitTurn(ctx context.Context, turn string) error {
	s.submits = append(s.submits, turn)
	return nil
}
func (s *stubSession) CancelTurn(ctx context.Context) error { return nil }
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

func (s *resumeOnlyStore) OpenSession(ctx context.Context, cwd, model, branch string) (storage.Session, error) {
	return nil, nil
}

func (s *resumeOnlyStore) ResumeSession(ctx context.Context, id string) (storage.Session, error) {
	return s.resumed, nil
}

func (s *resumeOnlyStore) ListSessions(ctx context.Context, cwd string) ([]storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) GetRecentSession(ctx context.Context, cwd string) (*storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) AddInput(ctx context.Context, cwd, content string) error { return nil }

func (s *resumeOnlyStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	return nil, nil
}

func (s *resumeOnlyStore) UpdateSession(ctx context.Context, si storage.SessionInfo) error {
	return nil
}

func (s *resumeOnlyStore) SaveKnowledge(ctx context.Context, item storage.KnowledgeItem) error {
	return nil
}

func (s *resumeOnlyStore) SearchKnowledge(ctx context.Context, cwd, query string, limit int) ([]storage.KnowledgeItem, error) {
	return nil, nil
}

func (s *resumeOnlyStore) DeleteKnowledge(ctx context.Context, id string) error { return nil }

func (s *resumeOnlyStore) CoreStore() *memory.CoreStore { return nil }

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
	updated, _ = model.Update(session.AssistantDelta{Delta: "streamed reply"})
	model = updated.(Model)

	if model.pending == nil || model.pending.Content != "streamed reply" {
		t.Fatalf("expected pending streamed assistant entry, got %#v", model.pending)
	}

	updated, cmd := model.Update(session.AssistantMessage{})
	model = updated.(Model)

	if model.pending != nil {
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

	if model.pending == nil || model.pending.Role != session.Tool {
		t.Fatalf("expected pending tool entry")
	}

	updated, cmd := model.Update(session.ToolResult{
		ToolName: "bash",
		Result:   "ok",
	})
	model = updated.(Model)

	if model.pending != nil {
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
	if got := model.composer.Height(); got != minComposerHeight {
		t.Fatalf("expected initial composer height %d, got %d", minComposerHeight, got)
	}

	// 5 lines of text
	model.composer.SetValue("1\n2\n3\n4\n5")
	model.layout()

	// Should be 5
	if got := model.composer.Height(); got != 5 {
		t.Fatalf("expected composer height 5 for 5 lines, got %d", got)
	}

	// Over the max (10)
	model.composer.SetValue(strings.Repeat("line\n", 20))
	model.layout()

	if got := model.composer.Height(); got != maxComposerHeight {
		t.Fatalf("expected composer height to clamp to %d, got %d", maxComposerHeight, got)
	}
}

func TestProgressLineFitsWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	model.width = 28
	model.progress = stateError
	model.lastError = strings.Repeat("connection refused while reconnecting to the backend ", 3)

	if got := lipgloss.Width(model.progressLine()); got > model.width {
		t.Fatalf("expected progress line width <= %d, got %d: %q", model.width, got, model.progressLine())
	}
}

func TestStatusLineFitsWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 32, Height: 24})
	model = updated.(Model)
	model.backend = stubBackend{
		sess:         &stubSession{events: make(chan session.Event)},
		provider:     "subscription-provider-with-a-very-long-name",
		model:        "model-name-that-would-wrap-in-a-small-terminal",
		contextLimit: 128000,
	}
	model.tokensSent = 45123
	model.tokensReceived = 78210
	model.totalCost = 0.042
	model.workdir = "/Users/nick/github/nijaru/ion"
	model.branch = "feature/resize-persistence"

	if got := lipgloss.Width(model.statusLine()); got > model.width {
		t.Fatalf("expected status line width <= %d, got %d: %q", model.width, got, model.statusLine())
	}
}

func TestStatusLineHidesZeroUsageBeforeFirstTurn(t *testing.T) {
	model := readyModel(t)
	model.tokensSent = 0
	model.tokensReceived = 0
	model.totalCost = 0
	model.backend = stubBackend{sess: &stubSession{events: make(chan session.Event)}}

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "0 tokens") {
		t.Fatalf("status line should hide zero usage, got %q", line)
	}
	if strings.Contains(line, "k/") {
		t.Fatalf("status line should not show context usage without turns, got %q", line)
	}
}

func TestComposerLayoutResetsAfterClear(t *testing.T) {
	model := readyModel(t)
	model.composer.SetValue("one\ntwo\nthree")
	model.layout()

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	model = updated.(Model)

	if got := model.composer.Value(); got != "" {
		t.Fatalf("expected composer to be cleared, got %q", got)
	}
	if got := model.composer.Height(); got != minComposerHeight {
		t.Fatalf("expected composer height to reset to %d, got %d", minComposerHeight, got)
	}
}

func TestComposerLayoutReflowsAfterHistoryRecall(t *testing.T) {
	model := readyModel(t)
	model.history = []string{"first\nsecond\nthird"}

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = updated.(Model)

	if got := model.composer.Value(); got != "first\nsecond\nthird" {
		t.Fatalf("expected recalled history entry, got %q", got)
	}
	if got := model.composer.Height(); got != 3 {
		t.Fatalf("expected composer height to expand to 3, got %d", got)
	}
}

func TestHandleCommandUpdatesConfigDirectly(t *testing.T) {
	tests := []struct {
		name     string
		command  string
		expected string
	}{
		{name: "provider", command: "/provider anthropic", expected: "provider = 'anthropic'\nsession_retention_days = 90\n"},
		{name: "model", command: "/model gpt-4.1", expected: "model = 'gpt-4.1'\nsession_retention_days = 90\n"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			home := t.TempDir()
			t.Setenv("HOME", home)

			oldSession := &stubSession{events: make(chan session.Event)}
			oldBackend := stubBackend{sess: oldSession}
			model := New(oldBackend, nil, nil, "/tmp/test", "main", "dev", nil)

			cmd := model.handleCommand(tc.command)
			if cmd == nil {
				t.Fatal("expected transcript notice command")
			}
			if model.picker != nil {
				t.Fatal("expected no picker to open")
			}

			data, err := os.ReadFile(filepath.Join(home, ".ion", "config.toml"))
			if err != nil {
				t.Fatalf("read config: %v", err)
			}
			if got := string(data); got != tc.expected {
				t.Fatalf("config = %q, want %q", got, tc.expected)
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

	cmd := model.handleCommand("/compact")
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

	msg := model.handleCommand("/compact")()
	compacted, ok := msg.(sessionCompactedMsg)
	if !ok {
		t.Fatalf("expected sessionCompactedMsg, got %T", msg)
	}
	if compacted.notice != "Session is already within compaction limits" {
		t.Fatalf("compact no-op notice = %q", compacted.notice)
	}
}

func TestCompactCommandErrorsWhenBackendUnsupported(t *testing.T) {
	model := New(stubBackend{sess: &stubSession{events: make(chan session.Event)}}, nil, nil, "/tmp/test", "main", "dev", nil)

	msg := model.handleCommand("/compact")()
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
	model := New(oldBackend, nil, nil, "/tmp/test", "main", "dev", func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
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
	})

	cmd := model.handleCommand("/clear")
	if cmd == nil {
		t.Fatal("expected /clear command to return a cmd")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	if observedSessionID != "" {
		t.Fatalf("session ID passed to clear switcher = %q, want empty for fresh session", observedSessionID)
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
	oldBackend := stubBackend{sess: oldSession, provider: "openrouter", model: "deepseek/deepseek-v3.2"}

	model := New(oldBackend, nil, nil, "/tmp/test", "main", "dev", func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
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
	})

	msg := model.handleCommand("/clear")()
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

	msg := model.handleCommand("/cost")()
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

	msg := model.handleCommand("/cost")()
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

	msg := model.handleCommand("/help")()
	helpMsg, ok := msg.(sessionHelpMsg)
	if !ok {
		t.Fatalf("expected sessionHelpMsg, got %T", msg)
	}

	for _, want := range []string{
		"/resume [id]",
		"/provider [name]",
		"/model [name]",
		"/compact",
		"/clear",
		"/cost",
		"/mcp add <cmd>",
		"/quit, /exit",
		"/help",
		"Ctrl+P",
		"Ctrl+M",
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
	items := providerItems()

	for label, wantDetail := range map[string]string{
		"anthropic":       "API key missing",
		"openrouter":      "API key set",
		"claude-pro":      "ACP",
		"gemini-advanced": "ACP",
		"gh-copilot":      "ACP",
		"chatgpt":         "ACP",
		"codex":           "ACP",
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
	oldListModels := listModels
	listModels = func(ctx context.Context, provider string) ([]registry.ModelMetadata, error) {
		if provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", provider)
		}
		return []registry.ModelMetadata{
			{ID: "b-model", ContextLimit: 64000, InputPrice: 1.23, OutputPrice: 4.56},
			{ID: "a-model", ContextLimit: 128000, InputPrice: 0.1, OutputPrice: 0.2},
		}, nil
	}
	defer func() { listModels = oldListModels }()

	items, err := modelItemsForProvider("openrouter")
	if err != nil {
		t.Fatalf("modelItemsForProvider: %v", err)
	}
	if len(items) != 2 {
		t.Fatalf("items len = %d, want 2", len(items))
	}
	if items[0].Label != "a-model" || items[1].Label != "b-model" {
		t.Fatalf("items not sorted by label: %#v", items)
	}
	if !strings.Contains(items[0].Detail, "128k ctx") || !strings.Contains(items[0].Detail, "$0.1000/$0.2000") {
		t.Fatalf("unexpected model detail: %q", items[0].Detail)
	}
}

func TestPickerFilteringMatchesTypedQuery(t *testing.T) {
	model := readyModel(t)
	model.picker = &pickerState{
		title: "Pick a provider",
		items: []pickerItem{
			{Label: "anthropic", Value: "anthropic", Detail: "API key missing"},
			{Label: "openrouter", Value: "openrouter", Detail: "API key set"},
		},
		filtered: []pickerItem{
			{Label: "anthropic", Value: "anthropic", Detail: "API key missing"},
			{Label: "openrouter", Value: "openrouter", Detail: "API key set"},
		},
		purpose: pickerPurposeProvider,
	}

	for _, r := range []rune("router") {
		model, _ = model.handlePickerKey(tea.KeyPressMsg{Text: string(r), Code: r})
	}

	if got := len(pickerDisplayItems(model.picker)); got != 1 {
		t.Fatalf("filtered items = %d, want 1", got)
	}
	if got := pickerDisplayItems(model.picker)[0].Label; got != "openrouter" {
		t.Fatalf("filtered label = %q, want openrouter", got)
	}
}

func TestQueuedFollowUpSubmitsAfterTurnFinished(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.session = sess
	model.composer.SetValue("follow up")
	model.thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)
	if model.queuedTurn != "follow up" {
		t.Fatalf("queuedTurn = %q, want queued follow up", model.queuedTurn)
	}
	if got := model.composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after queueing", got)
	}
	if cmd == nil {
		t.Fatal("expected queue notice command")
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
		t.Fatal("expected submit turn command after queued turn message")
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
	model := New(oldBackend, nil, nil, "/tmp/test", "main", "dev", func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
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
	})

	model.picker = &pickerState{
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
	if got := model.backend.Provider(); got != "openai" {
		t.Fatalf("backend provider = %q, want %q", got, "openai")
	}
	if got := model.backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model = %q, want %q", got, "gpt-4.1")
	}
	if got := model.session.ID(); got != oldSession.ID() {
		t.Fatalf("session ID = %q, want %q", got, oldSession.ID())
	}
	if got := model.storage.ID(); got != oldSession.ID() {
		t.Fatalf("storage session ID = %q, want %q", got, oldSession.ID())
	}
	if got := model.branch; got != "feature/switch" {
		t.Fatalf("branch = %q, want %q", got, "feature/switch")
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
	model := New(oldBackend, nil, nil, "/tmp/test", "main", "dev", func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
		resolved := *cfg
		newBackend := testutil.New()
		newBackend.SetConfig(&resolved)
		newBackend.SetSession(newStorage)
		return newBackend, newBackend.Session(), newStorage, nil
	})

	next, _ := model.Update(runtimeSwitchedMsg{
		backend: testutil.New(),
		session: testutil.New(),
		storage: newStorage,
		status:  "ready",
		notice:  "Switched model to gpt-4.1",
	})
	model = next.(Model)

	if len(newStorage.appends) != 0 {
		t.Fatalf("expected runtime switch notice to stay out of transcript storage, got %d appends", len(newStorage.appends))
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
	model.startupLines = []string{"line-1", "line-2"}
	model.status = "ready"
	model.startupEntries = []session.Entry{
		{Role: session.User, Content: "hello"},
		{Role: session.Assistant, Content: "world"},
	}

	lines := model.startupPrintLines()
	want := []string{
		"line-1",
		"line-2",
		model.headerLine(),
		"",
		model.renderStartupStatus("ready"),
		model.renderEntry(session.Entry{Role: session.User, Content: "hello"}),
		model.renderEntry(session.Entry{Role: session.Assistant, Content: "world"}),
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

func TestAssistantEntryRendersMarkdown(t *testing.T) {
	model := readyModel(t)
	model.width = 80

	rendered := ansi.Strip(model.renderEntry(session.Entry{
		Role:    session.Assistant,
		Content: "# Heading\n\n- first item\n- second item\n\n| Name | Value |\n|------|-------|\n| foo  | 123   |\n\n```go\nfmt.Println(\"hi\")\n```",
	}))

	if strings.Contains(rendered, "```") {
		t.Fatalf("expected code fences to be rendered away, got %q", rendered)
	}
	if strings.Contains(rendered, "# Heading") {
		t.Fatalf("expected heading marker to be rendered away, got %q", rendered)
	}
	for _, want := range []string{"Heading", "first item", "second item", "foo", "123", "fmt.Println(\"hi\")"} {
		if !strings.Contains(rendered, want) {
			t.Fatalf("rendered markdown missing %q: %q", want, rendered)
		}
	}
}

func TestSessionPickerScopesToWorkspace(t *testing.T) {
	tmpRoot := filepath.Join(t.TempDir(), ".ion")
	store, err := storage.NewCantoStore(tmpRoot)
	if err != nil {
		t.Fatalf("failed to create store: %v", err)
	}

	cwd := "/tmp/workspace-a"
	other := "/tmp/workspace-b"

	sessionA, err := store.OpenSession(context.Background(), cwd, "openrouter/deepseek/deepseek-v3.2", "main")
	if err != nil {
		t.Fatalf("open workspace session: %v", err)
	}
	if err := sessionA.Append(context.Background(), storage.User{Type: "user", Content: "plan the feature", TS: now()}); err != nil {
		t.Fatalf("append workspace session: %v", err)
	}

	sessionB, err := store.OpenSession(context.Background(), other, "openrouter/minimax/minimax-m2.7", "main")
	if err != nil {
		t.Fatalf("open other session: %v", err)
	}
	if err := sessionB.Append(context.Background(), storage.User{Type: "user", Content: "other workspace", TS: now()}); err != nil {
		t.Fatalf("append other session: %v", err)
	}

	model := New(stubBackend{sess: &stubSession{events: make(chan session.Event)}}, nil, store, cwd, "main", "dev", nil)
	if cmd := model.openSessionPicker(); cmd != nil {
		t.Fatalf("expected no command from openSessionPicker, got %T", cmd)
	}
	if model.sessionPicker == nil {
		t.Fatal("expected session picker state")
	}
	if got := len(model.sessionPicker.items); got != 1 {
		t.Fatalf("session picker items = %d, want 1", got)
	}
	if got := model.sessionPicker.items[0].info.ID; got != sessionA.ID() {
		t.Fatalf("session picker showed %q, want %q", got, sessionA.ID())
	}
}

func TestSessionPickerLineUsesPreviewAndAge(t *testing.T) {
	info := storage.SessionInfo{
		ID:          "session-123",
		LastPreview: "refactor the picker overlay",
		UpdatedAt:   time.Now().Add(-2 * time.Hour),
	}

	label, detail := sessionPickerLine("/tmp/workspace-a", info)
	if label != "refactor the picker overlay" {
		t.Fatalf("label = %q, want preview text", label)
	}
	if !strings.Contains(detail, "ago") {
		t.Fatalf("detail %q missing age", detail)
	}
}
