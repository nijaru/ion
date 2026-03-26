package app

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/internal/backend"
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

type stubSession struct {
	events chan session.Event
}

func (s *stubSession) Open(ctx context.Context) error                    { return nil }
func (s *stubSession) Resume(ctx context.Context, id string) error       { return nil }
func (s *stubSession) SubmitTurn(ctx context.Context, turn string) error { return nil }
func (s *stubSession) CancelTurn(ctx context.Context) error              { return nil }
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
	id     string
	model  string
	branch string
}

func (s *stubStorageSession) ID() string { return s.id }

func (s *stubStorageSession) Meta() storage.Metadata {
	return storage.Metadata{
		ID:     s.id,
		Model:  s.model,
		Branch: s.branch,
	}
}

func (s *stubStorageSession) Append(ctx context.Context, event any) error { return nil }

func (s *stubStorageSession) Entries(ctx context.Context) ([]session.Entry, error) {
	return nil, nil
}

func (s *stubStorageSession) LastStatus(ctx context.Context) (string, error) { return "", nil }

func (s *stubStorageSession) Usage(ctx context.Context) (int, int, float64, error) {
	return 0, 0, 0, nil
}

func (s *stubStorageSession) Close() error { return nil }

func readyModel(t *testing.T) Model {
	t.Helper()
	sess := &stubSession{events: make(chan session.Event)}
	b := stubBackend{sess: sess}
	model := New(b, nil, "/tmp/test", "main", "dev", nil)
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

func TestHandleCommandSwitchesRuntime(t *testing.T) {
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
	model := New(oldBackend, nil, "/tmp/test", "main", "dev", func(ctx context.Context, cfg *config.Config) (backend.Backend, session.AgentSession, storage.Session, error) {
		switched = true

		resolved := *cfg
		resolved.Provider = "openai"

		newStorage := &stubStorageSession{
			id:     "switched-session",
			model:  resolved.Model,
			branch: "feature/switch",
		}

		newBackend := testutil.New()
		newBackend.SetConfig(&resolved)
		newBackend.SetSession(newStorage)

		return newBackend, newBackend.Session(), newStorage, nil
	})

	cmd := model.handleCommand("/model gpt-4.1")
	msg := cmd()

	switchedMsg, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}

	updated, _ := model.Update(switchedMsg)
	model = updated.(Model)

	if !switched {
		t.Fatal("expected runtime switch callback to be invoked")
	}
	if got := model.backend.Provider(); got != "openai" {
		t.Fatalf("backend provider = %q, want %q", got, "openai")
	}
	if got := model.backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model = %q, want %q", got, "gpt-4.1")
	}
	if got := model.session.ID(); got != "switched-session" {
		t.Fatalf("session ID = %q, want %q", got, "switched-session")
	}
	if got := model.storage.ID(); got != "switched-session" {
		t.Fatalf("storage session ID = %q, want %q", got, "switched-session")
	}
	if got := model.branch; got != "feature/switch" {
		t.Fatalf("branch = %q, want %q", got, "feature/switch")
	}
}
