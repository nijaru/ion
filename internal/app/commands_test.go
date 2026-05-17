package app

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/testutil"
)

func TestHandleCommandUpdatesStateDirectly(t *testing.T) {
	tests := []struct {
		name        string
		command     string
		expected    string
		wantCommand bool
	}{
		{
			name:        "model",
			command:     "/model gpt-4.1",
			expected:    "model = 'gpt-4.1'\n",
			wantCommand: true,
		},
		{
			name:        "thinking",
			command:     "/thinking high",
			expected:    "reasoning_effort = 'high'\n",
			wantCommand: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			home := t.TempDir()
			t.Setenv("HOME", home)

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
			if model.Picker.Overlay != nil {
				t.Fatal("expected no picker to open")
			}

			data, err := os.ReadFile(filepath.Join(home, ".ion", "state.toml"))
			if err != nil {
				t.Fatalf("read state: %v", err)
			}
			if got := string(data); got != tc.expected {
				t.Fatalf("state = %q, want %q", got, tc.expected)
			}
			if model.Progress.Status == "" {
				t.Fatal("expected status to be updated after direct config command")
			}
		})
	}
}

func TestProviderCommandStagesListingProviderUntilModelSelection(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ANTHROPIC_API_KEY", "test-key")
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			if cfg.Provider != "anthropic" {
				t.Fatalf("provider = %q, want anthropic", cfg.Provider)
			}
			if cfg.Model != "" {
				t.Fatalf("model = %q, want staged provider without model", cfg.Model)
			}
			return []registry.ModelMetadata{{ID: "claude-test"}}, nil
		},
	)

	capture := &configCaptureBackend{
		stubBackend: stubBackend{
			sess:     &stubSession{events: make(chan session.Event)},
			provider: "openai",
			model:    "gpt-4.1",
		},
	}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil)

	updated, cmd := model.handleCommand("/provider anthropic")
	model = resolveModelPickerLoad(t, updated, cmd)

	if capture.cfg != nil {
		t.Fatalf("backend config = %#v, want provider staged only in picker", capture.cfg)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
	if got := model.Picker.Overlay.cfg.Provider; got != "anthropic" {
		t.Fatalf("picker provider = %q, want anthropic", got)
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "state.toml")); !os.IsNotExist(err) {
		t.Fatalf("state file error = %v, want provider unstored until model selection", err)
	}

	model, _ = model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEsc})
	if model.Picker.Overlay != nil {
		t.Fatal("expected picker to close after cancel")
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "state.toml")); !os.IsNotExist(err) {
		t.Fatalf("state file after cancel error = %v, want no staged provider persisted", err)
	}
	if got := model.Model.Backend.Provider(); got != "openai" {
		t.Fatalf("backend provider = %q, want unchanged openai", got)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model = %q, want unchanged gpt-4.1", got)
	}
}

func TestWithProviderPickerOpensSetupPicker(t *testing.T) {
	model := readyModel(t).WithProviderPicker()
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeProvider {
		t.Fatalf("picker = %#v, want provider picker", model.Picker.Overlay)
	}
}

func TestProviderCommandCurrentProviderKeepsConfiguredModel(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			if cfg.Provider != "openrouter" {
				t.Fatalf("provider = %q, want openrouter", cfg.Provider)
			}
			if cfg.Model != "z-ai/glm-5" {
				t.Fatalf("model = %q, want current model", cfg.Model)
			}
			return []registry.ModelMetadata{{ID: "z-ai/glm-5"}}, nil
		},
	)

	model := readyModel(t).WithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "z-ai/glm-5",
	})

	updated, cmd := model.handleCommand("/provider openrouter")
	model = resolveModelPickerLoad(t, updated, cmd)
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
	if got := model.Picker.Overlay.cfg.Model; got != "z-ai/glm-5" {
		t.Fatalf("picker model = %q, want current model", got)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[model.Picker.Overlay.index].Value; got != "z-ai/glm-5" {
		t.Fatalf("selected model = %q, want current model", got)
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "state.toml")); !os.IsNotExist(err) {
		t.Fatalf("state file error = %v, want provider/model unchanged until selection", err)
	}
}

func TestModelCommandDoesNotPersistStateWhenRuntimeSwitchFails(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(cfgDir, "config.toml"),
		[]byte("provider = \"openai\"\nmodel = \"gpt-4.1-old\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldBackend := stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openai",
		model:    "gpt-4.1-old",
	}
	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			return nil, nil, nil, errors.New("switch failed")
		},
	)

	updated, cmd := model.handleCommand("/model gpt-4.1-new")
	model = updated
	if cmd == nil {
		t.Fatal("expected model command to start runtime switch")
	}
	raw := cmd()
	switchErr, ok := raw.(runtimeSwitchErrorMsg)
	if !ok {
		t.Fatalf("switch command message = %T, want runtimeSwitchErrorMsg", raw)
	}
	next, _ := model.Update(switchErr)
	model = next.(Model)

	if got := model.Model.Backend.Model(); got != "gpt-4.1-old" {
		t.Fatalf("backend model = %q, want unchanged old model", got)
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "state.toml")); !os.IsNotExist(err) {
		t.Fatalf("state file error = %v, want no failed model persisted", err)
	}
}

func TestModelCommandWithoutSwitcherUpdatesAppConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(cfgDir, "config.toml"),
		[]byte("provider = \"openai\"\nmodel = \"gpt-4.1-old\"\nfast_model = \"gpt-4.1-mini\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}

	capture := &configCaptureBackend{
		stubBackend: stubBackend{
			sess:     &stubSession{events: make(chan session.Event)},
			provider: "openai",
			model:    "gpt-4.1-old",
		},
	}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil)

	updated, cmd := model.handleCommand("/model gpt-4.1-new")
	model = updated
	if cmd == nil {
		t.Fatal("expected model command notice")
	}
	if capture.cfg == nil || capture.cfg.Model != "gpt-4.1-new" {
		t.Fatalf("backend config = %#v, want new model", capture.cfg)
	}
	if model.Model.Config == nil ||
		model.Model.Config.Model != "gpt-4.1-new" ||
		model.Model.Config.FastModel != "gpt-4.1-mini" {
		t.Fatalf("app config = %#v, want updated full config", model.Model.Config)
	}
}

func TestProviderCommandRejectsInvalidProvidersBeforePersistingState(t *testing.T) {
	for _, tc := range []struct {
		name    string
		command string
		wantErr string
	}{
		{
			name:    "unknown",
			command: "/provider definitely-not-real",
			wantErr: `unsupported provider "definitely-not-real"`,
		},
		{
			name:    "acp",
			command: "/provider claude-pro",
			wantErr: "ACP providers are deferred",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			home := t.TempDir()
			t.Setenv("HOME", home)

			model := readyModel(t)
			model, cmd := model.handleCommand(tc.command)
			if cmd == nil {
				t.Fatal("expected provider command error")
			}
			if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("error = %v, want %q", err, tc.wantErr)
			}
			if model.Picker.Overlay != nil {
				t.Fatal("invalid provider should not open a picker")
			}
			statePath := filepath.Join(home, ".ion", "state.toml")
			if _, err := os.Stat(statePath); !os.IsNotExist(err) {
				t.Fatalf("state file error = %v, want no persisted provider state", err)
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

	model, cmd := model.handleCommand("/compact")
	if cmd == nil {
		t.Fatal("expected /compact command to return a cmd")
	}
	if !model.Progress.Compacting {
		t.Fatal("expected /compact to mark compaction in progress")
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

func TestCompactingStatusShowsProgressLine(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.StatusChanged{Status: "Compacting context..."})
	model = updated.(Model)

	if !model.Progress.Compacting {
		t.Fatal("expected compacting status to mark compaction in progress")
	}
	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "Compacting context...") {
		t.Fatalf("progress line = %q, want compaction status", line)
	}

	updated, _ = model.Update(session.StatusChanged{Status: "Ready"})
	model = updated.(Model)
	if model.Progress.Compacting {
		t.Fatal("expected ready status to clear compaction progress")
	}
}

func TestComposerQueuesWhileCompacting(t *testing.T) {
	model := readyModel(t)
	model.Progress.Compacting = true
	model.Input.Composer.SetValue("follow up")

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

func TestCompactCommandDoesNotMaterializeLazySession(t *testing.T) {
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	lazy := storage.NewLazySession(store, "/tmp/test", "openai/model-a", "main")
	backend := &compactBackend{
		stubBackend: stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		compacted:   true,
	}
	model := New(backend, lazy, store, "/tmp/test", "main", "dev", nil)

	model, cmd := model.handleCommand("/compact")
	if cmd == nil {
		t.Fatal("expected /compact command to return a notice")
	}
	if model.Progress.Compacting {
		t.Fatal("lazy /compact should not mark compaction in progress")
	}
	if backend.called {
		t.Fatal("lazy /compact called backend compactor before a session existed")
	}
	if storage.IsMaterialized(lazy) {
		t.Fatal("lazy /compact materialized a session")
	}
}

func TestCompactCompletionClearsStaleErrorState(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "stale provider error"
	model.Progress.Compacting = true

	updated, _ := model.Update(sessionCompactedMsg{notice: "Compacted current session context"})
	model = updated.(Model)

	if model.Progress.Compacting {
		t.Fatal("expected compaction progress to clear")
	}
	if model.Progress.Mode == stateError || model.Progress.LastError != "" {
		t.Fatalf(
			"progress error state = (%v, %q), want cleared",
			model.Progress.Mode, model.Progress.LastError,
		)
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
	err := localErrorFromMsg(t, msg)
	if err.Error() != "current backend does not support /compact" {
		t.Fatalf("unexpected /compact error: %v", err)
	}
}

func TestClearCommandStartsFreshSession(t *testing.T) {
	for _, tc := range []struct {
		command string
		notice  string
	}{
		{command: "/new", notice: "Started new session"},
		{command: "/clear", notice: "Started fresh session"},
	} {
		t.Run(tc.command, func(t *testing.T) {
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

			model, cmd := model.handleCommand(tc.command)
			if cmd == nil {
				t.Fatalf("expected %s command to return a cmd", tc.command)
			}
			msg := cmd()
			switched, ok := msg.(runtimeSwitchedMsg)
			if !ok {
				t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
			}
			if observedSessionID != "" {
				t.Fatalf(
					"session ID passed to fresh-session switcher = %q, want empty",
					observedSessionID,
				)
			}
			if switched.notice != tc.notice {
				t.Fatalf("%s notice = %q, want %q", tc.command, switched.notice, tc.notice)
			}
		})
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
		model:    "deepseek/deepseek-v4-flash",
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
			if cfg.Model != "deepseek/deepseek-v4-flash" {
				t.Fatalf("model = %q, want deepseek/deepseek-v4-flash", cfg.Model)
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

func TestSessionInfoNoticeReportsCurrentSession(t *testing.T) {
	model := New(
		stubBackend{
			sess:     &stubSession{events: make(chan session.Event)},
			provider: "openrouter",
			model:    "minimax/minimax-m2.5:free",
		},
		&stubStorageSession{
			id:        "sess-1",
			usageIn:   1200,
			usageOut:  300,
			usageCost: 0.012345,
			entries: []session.Entry{
				{Role: session.User, Content: "hi"},
				{Role: session.Agent, Content: "hello"},
				{Role: session.Tool, Title: "bash"},
			},
		},
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)
	notice, err := model.sessionInfoNotice()
	if err != nil {
		t.Fatalf("sessionInfoNotice returned error: %v", err)
	}
	for _, want := range []string{
		"Session",
		"id: sess-1",
		"provider: openrouter",
		"model: minimax/minimax-m2.5:free",
		"branch: main",
		"messages: user 1, assistant 1, tools 1, total 3",
		"tokens: input 1200, output 300, total 1500",
		"cost: $0.012345",
	} {
		if !strings.Contains(notice, want) {
			t.Fatalf("session notice missing %q: %q", want, notice)
		}
	}
}

func TestSessionInfoNoticeDoesNotMaterializeLazySession(t *testing.T) {
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	lazy := storage.NewLazySession(store, "/tmp/test", "openai/model-a", "main")
	model := New(
		stubBackend{
			sess:     &stubSession{events: make(chan session.Event)},
			provider: "openai",
			model:    "model-a",
		},
		lazy,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	notice, err := model.sessionInfoNotice()
	if err != nil {
		t.Fatalf("sessionInfoNotice returned error: %v", err)
	}
	if storage.IsMaterialized(lazy) {
		t.Fatal("session info materialized lazy session")
	}
	for _, want := range []string{
		"id: none",
		"provider: openai",
		"model: model-a",
		"messages: user 0, assistant 0, tools 0, total 0",
		"tokens: input 0, output 0, total 0",
	} {
		if !strings.Contains(notice, want) {
			t.Fatalf("session notice missing %q: %q", want, notice)
		}
	}
	recent, err := store.GetRecentSession(context.Background(), "/tmp/test")
	if err != nil {
		t.Fatalf("recent session: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent session after session info = %#v, want nil", recent)
	}
}

func TestCostCommandReportsConfiguredBudgets(t *testing.T) {
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		&stubStorageSession{usageIn: 1200, usageOut: 300, usageCost: 0.012345},
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)
	model.Model.Config = &config.Config{
		MaxSessionCost: 0.050000,
		MaxTurnCost:    0.010000,
	}

	_, cmd := model.handleCommand("/cost")
	msg := cmd()
	costMsg, ok := msg.(sessionCostMsg)
	if !ok {
		t.Fatalf("expected sessionCostMsg, got %T", msg)
	}
	for _, want := range []string{
		"session limit: $0.050000",
		"session remaining: $0.037655",
		"turn limit: $0.010000",
	} {
		if !strings.Contains(costMsg.notice, want) {
			t.Fatalf("cost notice missing %q: %q", want, costMsg.notice)
		}
	}
}

func TestBusyTurnBlocksRuntimeChangingCommands(t *testing.T) {
	commands := []string{
		"/primary",
		"/fast",
		"/resume session-1",
		"/model model-b",
		"/provider local-api",
		"/thinking high",
		"/settings retry on",
		"/new",
		"/clear",
		"/compact",
	}

	for _, command := range commands {
		t.Run(command, func(t *testing.T) {
			model := readyModel(t)
			model.InFlight.Thinking = true

			_, cmd := model.handleCommand(command)
			if cmd == nil {
				t.Fatal("expected busy command to return an error")
			}
			err := localErrorFromMsg(t, cmd())
			if !strings.Contains(err.Error(), "Finish or cancel the current turn") {
				t.Fatalf("error = %v, want busy-turn guard", err)
			}
		})
	}
}

func TestRuntimeSwitchBlocksRuntimeChangingCommands(t *testing.T) {
	commands := []string{
		"/primary",
		"/fast",
		"/resume session-1",
		"/model model-b",
		"/provider local-api",
		"/thinking high",
		"/settings retry on",
		"/new",
		"/clear",
		"/compact",
	}

	for _, command := range commands {
		t.Run(command, func(t *testing.T) {
			model := readyModel(t)
			model.Model.RuntimeSwitchRequest = 1

			_, cmd := model.handleCommand(command)
			if cmd == nil {
				t.Fatal("expected runtime-switch command to return an error")
			}
			err := localErrorFromMsg(t, cmd())
			if !strings.Contains(err.Error(), "runtime switch") {
				t.Fatalf("error = %v, want runtime-switch guard", err)
			}
		})
	}
}

func TestBusyTurnAllowsReadOnlyLocalCommands(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true

	for _, command := range []string{"/help", "/session", "/cost", "/tools"} {
		t.Run(command, func(t *testing.T) {
			_, cmd := model.handleCommand(command)
			if cmd == nil {
				t.Fatal("expected local command output")
			}
			if _, ok := cmd().(localErrorMsg); ok {
				t.Fatalf("%s should remain available while a turn is active", command)
			}
		})
	}
}

func TestRuntimeSwitchBlocksPresetHotkey(t *testing.T) {
	model := readyModel(t)
	model.Model.RuntimeSwitchRequest = 1
	model.App.ActivePreset = presetPrimary

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'm', Mod: tea.ModCtrl})
	model = updated.(Model)

	if cmd == nil {
		t.Fatal("expected runtime-switch guard error")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "runtime switch") {
		t.Fatalf("error = %v, want runtime-switch guard", err)
	}
	if model.App.ActivePreset != presetPrimary {
		t.Fatalf("active preset = %q, want unchanged primary", model.App.ActivePreset)
	}
}

func TestBusyTurnBlocksRuntimeChangingPickerSelection(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.Picker.Overlay = &pickerOverlayState{
		items: []pickerItem{
			{Label: "model-b", Value: "model-b"},
		},
		filtered: []pickerItem{
			{Label: "model-b", Value: "model-b"},
		},
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "local-api", Model: "model-a"},
	}

	updated, cmd := model.commitPickerSelection()
	model = updated
	if model.Picker.Overlay != nil {
		t.Fatal("expected busy picker selection to close overlay")
	}
	if cmd == nil {
		t.Fatal("expected busy picker selection to return an error")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "Finish or cancel the current turn") {
		t.Fatalf("error = %v, want busy-turn guard", err)
	}
}

func TestRuntimeSwitchBlocksRuntimeChangingPickerSelection(t *testing.T) {
	model := readyModel(t)
	model.Model.RuntimeSwitchRequest = 1
	model.Picker.Overlay = &pickerOverlayState{
		items: []pickerItem{
			{Label: "model-b", Value: "model-b"},
		},
		filtered: []pickerItem{
			{Label: "model-b", Value: "model-b"},
		},
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "local-api", Model: "model-a"},
	}

	updated, cmd := model.commitPickerSelection()
	model = updated
	if model.Picker.Overlay != nil {
		t.Fatal("expected runtime-switch picker selection to close overlay")
	}
	if cmd == nil {
		t.Fatal("expected runtime-switch picker selection to return an error")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "runtime switch") {
		t.Fatalf("error = %v, want runtime-switch guard", err)
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
	if cmd == nil {
		t.Fatal("expected /help command")
	}
	notice := helpText()

	wantCommands := []string{
		"/help",
		"/new",
		"/clear",
		"/resume [id]",
		"/session",
		"/compact",
		"/provider [name]",
		"/model [name]",
		"/thinking [lvl]",
		"/primary",
		"/fast",
		"/settings",
		"/tools",
		"/skills [query]",
		"/cost",
		"/status",
		"/quit, /exit",
	}
	wantCommands = append(
		wantCommands,
		"Ctrl+P",
		"Ctrl+X",
		"Tab",
		"Esc",
		"Up / Down",
		"Enter",
		"Ctrl+C           clear composer, cancel running turn",
		"Ctrl+D           delete forward",
	)
	for _, want := range wantCommands {
		if !strings.Contains(notice, want) {
			t.Fatalf("help notice missing %q: %q", want, notice)
		}
	}
	for _, disabled := range []string{
		"/rewind <id>",
		"/mcp add <cmd>",
		"lazy loading",
	} {
		if strings.Contains(notice, disabled) {
			t.Fatalf(
				"help notice should not advertise deferred/internal surface %q: %q",
				disabled,
				notice,
			)
		}
	}
	for _, hidden := range []string{
		"/read",
		"/edit",
		"/auto, /yolo",
		"/trust [status]",
		"/fork [label]",
		"/tree",
		"/jobs",
		"/stop <job-id>",
	} {
		if strings.Contains(notice, hidden) {
			t.Fatalf(
				"help notice should not advertise hidden command %q: %q",
				hidden,
				notice,
			)
		}
	}
}

func TestDeferredAdvancedCommandsAreDisabled(t *testing.T) {
	model := readyModel(t)
	for _, input := range []string{
		"/fork experiment",
		"/tree",
		"/jobs",
		"/stop bash-1",
		"/mcp add server",
		"/rewind cp-1",
	} {
		t.Run(input, func(t *testing.T) {
			_, cmd := model.handleCommand(input)
			if cmd == nil {
				t.Fatalf("%s returned nil cmd", input)
			}
			err := localErrorFromMsg(t, cmd())
			if !strings.Contains(
				err.Error(),
				"deferred until its roadmap phase",
			) {
				t.Fatalf("%s error = %v", input, err)
			}
		})
	}
}

func TestHelpSectionDetectionIncludesCommands(t *testing.T) {
	for _, line := range []string{"commands", "keys"} {
		if !isHelpSectionLine(line) {
			t.Fatalf("isHelpSectionLine(%q) = false, want true", line)
		}
	}
	if isHelpSectionLine("approval") {
		t.Fatal("isHelpSectionLine(\"approval\") = true, want false")
	}
}

func TestRenderHelpLineStylesLabelsWithoutChangingText(t *testing.T) {
	model := readyModel(t)
	line := "  /resume [id]     resume a recent session or pick one"

	got := ansi.Strip(model.renderHelpLine(1, line))
	if got != line {
		t.Fatalf("renderHelpLine = %q, want %q", got, line)
	}
}

func TestSplitHelpDetail(t *testing.T) {
	key, sep, detail, ok := splitHelpDetail("  Ctrl+P / Ctrl+N  command history")
	if !ok {
		t.Fatal("splitHelpDetail did not split help row")
	}
	if key != "Ctrl+P / Ctrl+N" || sep != "  " || detail != "command history" {
		t.Fatalf("key=%q sep=%q detail=%q", key, sep, detail)
	}
}

func TestSettingsCommandShowsCommonSettings(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "config.toml"),
		[]byte("tool_verbosity = \"collapsed\"\nthinking_verbosity = \"hidden\"\nretry_until_cancelled = false\n"),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}

	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "tencent/hy3-preview:free",
	}
	model.Model.Config = &config.Config{
		Provider:        "openrouter",
		Model:           "tencent/hy3-preview:free",
		ReasoningEffort: "high",
	}
	cfg, err := config.LoadStable()
	if err != nil {
		t.Fatalf("load stable config: %v", err)
	}
	got := model.settingsSummary(cfg)
	for _, want := range []string{
		"retry network errors: off",
		"tool display: collapsed",
		"read output: summary",
		"write output: summary",
		"bash output: hidden",
		"thinking output: hidden",
		"busy input: queue",
		"/settings retry on|off",
		"/settings read full|summary|hidden",
		"/settings write diff|summary|hidden",
		"/settings bash full|summary|hidden",
		"/settings busy queue|steer",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("settings missing %q: %q", want, got)
		}
	}
	for _, unwanted := range []string{
		"provider: openrouter",
		"model: tencent/hy3-preview:free",
		"preset: primary",
		"thinking level: high",
		"/thinking auto|off|minimal|low|medium|high|xhigh",
	} {
		if strings.Contains(got, unwanted) {
			t.Fatalf("settings should not include runtime identity %q: %q", unwanted, got)
		}
	}
}

func TestSettingsCommandShowsDisplayDefaults(t *testing.T) {
	model := readyModel(t)
	got := model.settingsSummary(&config.Config{})
	for _, want := range []string{
		"tool display: auto",
		"read output: summary",
		"write output: summary",
		"bash output: hidden",
		"thinking output: hidden",
		"busy input: queue",
		"/settings tool auto|full|collapsed|hidden",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("settings missing %q: %q", want, got)
		}
	}
}

func TestSettingsCommandUpdatesDisplayOutputs(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}

	model := readyModel(t)
	model, cmd := model.handleCommand("/settings read full")
	if cmd == nil {
		t.Fatal("expected read setting command")
	}
	_ = cmd()
	model, cmd = model.handleCommand("/settings write summary")
	if cmd == nil {
		t.Fatal("expected write setting command")
	}
	_ = cmd()
	model, cmd = model.handleCommand("/settings bash summary")
	if cmd == nil {
		t.Fatal("expected bash setting command")
	}
	_ = cmd()
	model, cmd = model.handleCommand("/settings thinking collapsed")
	if cmd == nil {
		t.Fatal("expected thinking setting command")
	}
	_ = cmd()

	data, err := os.ReadFile(filepath.Join(configDir, "config.toml"))
	if err != nil {
		t.Fatalf("read config: %v", err)
	}
	got := string(data)
	for _, want := range []string{
		"read_output = 'full'",
		"write_output = 'summary'",
		"bash_output = 'summary'",
		"thinking_verbosity = 'collapsed'",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("config missing %q:\n%s", want, got)
		}
	}
	if model.Model.Config.ReadOutput != "full" ||
		model.Model.Config.WriteOutput != "summary" ||
		model.Model.Config.BashOutput != "summary" ||
		model.Model.Config.ThinkingVerbosity != "collapsed" {
		t.Fatalf(
			"runtime config read/write/bash/thinking output = %q/%q/%q/%q, want full/summary/summary/collapsed",
			model.Model.Config.ReadOutput,
			model.Model.Config.WriteOutput,
			model.Model.Config.BashOutput,
			model.Model.Config.ThinkingVerbosity,
		)
	}
}

func TestSettingsCommandUpdatesStableConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "state.toml"),
		[]byte("provider = \"local-api\"\nmodel = \"qwen3.6:27b\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	sess := &stubSession{events: make(chan session.Event)}
	capture := &configCaptureBackend{stubBackend: stubBackend{sess: sess}}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil)
	model.Model.Config = &config.Config{}
	model, cmd := model.handleCommand("/settings retry off")
	if cmd == nil {
		t.Fatal("expected settings command")
	}
	_ = cmd()

	data, err := os.ReadFile(filepath.Join(configDir, "config.toml"))
	if err != nil {
		t.Fatalf("read config: %v", err)
	}
	got := string(data)
	if !strings.Contains(got, "retry_until_cancelled = false") {
		t.Fatalf("config missing retry setting:\n%s", got)
	}
	if strings.Contains(got, "local-api") || strings.Contains(got, "qwen3.6:27b") {
		t.Fatalf("settings command leaked mutable state into config:\n%s", got)
	}
	if model.Model.Config == nil || model.Model.Config.RetryUntilCancelledEnabled() {
		t.Fatal("model config retry setting was not updated")
	}
	if model.Model.Config.Provider != "openai-compatible" ||
		model.Model.Config.Model != "qwen3.6:27b" {
		t.Fatalf(
			"runtime config = %s/%s, want state-backed openai-compatible/qwen3.6:27b",
			model.Model.Config.Provider,
			model.Model.Config.Model,
		)
	}
	if capture.cfg == nil || capture.cfg.Provider != "openai-compatible" ||
		capture.cfg.Model != "qwen3.6:27b" {
		t.Fatalf("backend config = %#v, want state-backed provider/model", capture.cfg)
	}
}

func TestSettingsCommandPreservesRuntimeSelection(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "state.toml"),
		[]byte("provider = \"local-api\"\nmodel = \"qwen3.6:27b\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	sess := &stubSession{events: make(chan session.Event)}
	capture := &configCaptureBackend{stubBackend: stubBackend{sess: sess}}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil).
		WithConfig(&config.Config{
			Provider: "openrouter",
			Model:    "tencent/hy3-preview:free",
		})
	model, cmd := model.handleCommand("/settings tool collapsed")
	if cmd == nil {
		t.Fatal("expected settings command")
	}
	_ = cmd()

	data, err := os.ReadFile(filepath.Join(configDir, "config.toml"))
	if err != nil {
		t.Fatalf("read config: %v", err)
	}
	got := string(data)
	if !strings.Contains(got, "tool_verbosity = 'collapsed'") {
		t.Fatalf("config missing tool verbosity:\n%s", got)
	}
	if strings.Contains(got, "tencent/hy3-preview:free") {
		t.Fatalf("settings command leaked runtime model into stable config:\n%s", got)
	}
	if model.Model.Config == nil {
		t.Fatal("model config missing")
	}
	if model.Model.Config.Provider != "openrouter" ||
		model.Model.Config.Model != "tencent/hy3-preview:free" ||
		model.Model.Config.ToolVerbosity != "collapsed" {
		t.Fatalf(
			"runtime config = %#v, want runtime selection plus updated setting",
			model.Model.Config,
		)
	}
	if capture.cfg == nil ||
		capture.cfg.Provider != "openrouter" ||
		capture.cfg.Model != "tencent/hy3-preview:free" ||
		capture.cfg.ToolVerbosity != "collapsed" {
		t.Fatalf("backend config = %#v, want runtime selection plus updated setting", capture.cfg)
	}
}

func TestSettingsToolAutoClearsStableOverride(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "config.toml"),
		[]byte("tool_verbosity = \"full\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}

	model := readyModel(t)
	model, cmd := model.handleCommand("/settings tool auto")
	if cmd == nil {
		t.Fatal("expected settings command")
	}
	_ = cmd()

	data, err := os.ReadFile(filepath.Join(configDir, "config.toml"))
	if err != nil {
		t.Fatalf("read config: %v", err)
	}
	if strings.Contains(string(data), "tool_verbosity") {
		t.Fatalf("config kept tool override after auto:\n%s", data)
	}
	if model.Model.Config.ToolVerbosity != "" {
		t.Fatalf("runtime tool verbosity = %q, want auto/empty", model.Model.Config.ToolVerbosity)
	}
}

func TestToolsCommandReportsToolSurface(t *testing.T) {
	model := readyModel(t)

	_, cmd := model.handleCommand("/tools")
	if cmd == nil {
		t.Fatal("tools command returned nil cmd")
	}
}

func TestStatusCommandReportsRuntimePosture(t *testing.T) {
	model := readyModel(t)
	model.App.Sandbox = "auto: seatbelt"

	_, cmd := model.handleCommand("/status")
	if cmd == nil {
		t.Fatal("status command returned nil cmd")
	}
	got := runtimeStatusSummary(model)
	for _, want := range []string{
		"Permissions: trusted by default",
		"Provider: stub",
		"Model: stub-model",
		"Tools: 2",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("status = %q, want %q", got, want)
		}
	}
}

func TestToolSurfaceSummaryIncludesEnvironment(t *testing.T) {
	got := toolSurfaceSummary(backend.ToolSurface{
		Count:       2,
		Names:       []string{"bash", "read"},
		Sandbox:     "off",
		Environment: "inherit",
	})
	if !strings.Contains(got, "sandbox off") || !strings.Contains(got, "bash env inherited") {
		t.Fatalf("summary = %q, want sandbox and environment posture", got)
	}
	if strings.Contains(got, "eager") {
		t.Fatalf("summary = %q, want no internal eager/lazy jargon for default tools", got)
	}
}

func TestToolSurfaceSummaryReportsLazyWhenEnabled(t *testing.T) {
	got := toolSurfaceSummary(backend.ToolSurface{
		Count:         25,
		Names:         []string{"bash", "read"},
		LazyEnabled:   true,
		LazyThreshold: 20,
	})
	if !strings.Contains(got, "search tools enabled above 20") {
		t.Fatalf("summary = %q, want lazy tool notice", got)
	}
}
