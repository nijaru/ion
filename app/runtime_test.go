package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/testutil"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/internal/core"
)

func TestNewRestoresActivePresetFromState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := config.SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}

	model := readyModel(t)
	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast", model.App.ActivePreset)
	}
}

func TestResumeSessionIDUsesMaterializedStorage(t *testing.T) {
	model := readyModel(t)
	model.Model.Storage = session.NewLazySession(&resumeOnlyStore{}, "/tmp/test", "stub", "main")
	if got := model.ResumeSessionID(); got != "" {
		t.Fatalf("resume session id = %q, want empty for lazy unmaterialized storage", got)
	}

	model.Model.Storage = &stubStorageSession{id: "session-1"}
	if got := model.ResumeSessionID(); got != "session-1" {
		t.Fatalf("resume session id = %q, want materialized storage id", got)
	}

	model.Model.Session = nil
	if got := model.ResumeSessionID(); got != "" {
		t.Fatalf("resume session id = %q, want empty without active session", got)
	}
}

func TestWithConfigForRuntimePresetKeepsAppConfigAndAppliesRuntimeConfig(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	sess := &stubSession{events: make(chan session.AgentEvent)}
	capture := &configCaptureBackend{stubBackend: stubBackend{sess: sess}}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil).
		WithConfigForRuntimePreset(
			&config.Config{
				Provider:            "openai",
				Model:               "gpt-4.1",
				ReasoningEffort:     "high",
				FastModel:           "gpt-4.1-mini",
				FastReasoningEffort: "low",
			},
			&config.Config{
				Provider:        "openai",
				Model:           "gpt-4.1-mini",
				ReasoningEffort: "low",
			},
			"fast",
		)

	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast", model.App.ActivePreset)
	}
	if model.Model.Config == nil || model.Model.Config.Model != "gpt-4.1" ||
		model.Model.Config.FastModel != "gpt-4.1-mini" {
		t.Fatalf("app config = %#v, want full preset config", model.Model.Config)
	}
	if capture.cfg == nil || capture.cfg.Model != "gpt-4.1-mini" ||
		capture.cfg.ReasoningEffort != "low" {
		t.Fatalf("backend cfg = %#v, want resolved fast runtime config", capture.cfg)
	}
	if model.Progress.ReasoningEffort != "low" {
		t.Fatalf("progress reasoning = %q, want low", model.Progress.ReasoningEffort)
	}
}

func TestRuntimeSwitchAppliesAppAndRuntimeSnapshotSeparately(t *testing.T) {
	capture := &configCaptureBackend{
		stubBackend: stubBackend{
			sess: &stubSession{events: make(chan session.AgentEvent)},
		},
	}
	model := readyModel(t)

	updated, _ := model.Update(runtimeSwitchMsgForTest(
		&config.Config{
			Provider:            "openai",
			Model:               "gpt-4.1",
			ReasoningEffort:     "high",
			FastModel:           "gpt-4.1-mini",
			FastReasoningEffort: "low",
		},
		&config.Config{
			Provider:        "openai",
			Model:           "gpt-4.1-mini",
			ReasoningEffort: "low",
		},
		presetFast,
		"ready",
		capture,
		&stubSession{events: make(chan session.AgentEvent)},
		&stubStorageSession{id: "session-1", branch: "main"},
	))
	model = testModel(t, updated)

	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast", model.App.ActivePreset)
	}
	if model.Model.Config == nil ||
		model.Model.Config.Model != "gpt-4.1" ||
		model.Model.Config.FastModel != "gpt-4.1-mini" {
		t.Fatalf("app config = %#v, want full app snapshot", model.Model.Config)
	}
	if capture.cfg == nil ||
		capture.cfg.Model != "gpt-4.1-mini" ||
		capture.cfg.ReasoningEffort != "low" {
		t.Fatalf("backend config = %#v, want resolved runtime snapshot", capture.cfg)
	}
	if model.Progress.ReasoningEffort != "low" {
		t.Fatalf("progress reasoning = %q, want runtime reasoning", model.Progress.ReasoningEffort)
	}
}

func runtimeSwitchMsgForTest(
	appCfg *config.Config,
	runtimeCfg *config.Config,
	preset core.Preset,
	status string,
	backend core.Backend,
	sess session.AgentSession,
	storageSess session.SessionHandle,
) runtimeSwitchedMsg {
	return runtimeSwitchedMsg{
		runtime: newAcceptedRuntime(
			newRuntimeTransition(appCfg, runtimeCfg, preset, status),
			core.Handles{
				Backend: backend,
				Session: sess,
				Storage: storageSess,
			},
		),
	}
}

func TestRuntimeSwitchAcceptedSnapshotIncludesRuntimeMetadata(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(runtimeSwitchMsgForTest(
		&config.Config{
			Provider:            "openai",
			Model:               "gpt-4.1",
			ReasoningEffort:     "high",
			FastModel:           "gpt-4.1-mini",
			FastReasoningEffort: "low",
		},
		&config.Config{
			Provider:        "openai",
			Model:           "gpt-4.1-mini",
			ReasoningEffort: "low",
		},
		presetFast,
		"ready",
		stubBackend{
			sess:     &stubSession{events: make(chan session.AgentEvent)},
			provider: "openai",
			model:    "gpt-4.1-mini",
		},
		&stubSession{events: make(chan session.AgentEvent)},
		&stubStorageSession{id: "session-1", branch: "main"},
	))
	model = testModel(t, updated)

	snapshot := model.Model.Runtime
	if snapshot.Provider != "openai" ||
		snapshot.Model != "gpt-4.1-mini" ||
		snapshot.Reasoning != "low" ||
		snapshot.Preset != presetFast ||
		snapshot.SessionID != "session-1" ||
		!snapshot.Materialized {
		t.Fatalf("runtime snapshot = %#v, want accepted runtime metadata", snapshot)
	}
	if got := runtimeStatusSummary(model); !strings.Contains(got, "Model: gpt-4.1-mini") {
		t.Fatalf("status = %q, want accepted runtime model", got)
	}
}

func TestRuntimeSwitchReturnsBeforeUsageLoadCompletes(t *testing.T) {
	storageSess := &blockingSessionInfoStorage{
		stubStorageSession: stubStorageSession{
			id:       "session-1",
			model:    "openai/gpt-4.1",
			usageIn:  1200,
			usageOut: 300,
		},
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	model := readyModel(t)

	type updateResult struct {
		model Model
		cmd   tea.Cmd
	}
	returned := make(chan updateResult, 1)
	go func() {
		updated, cmd := model.Update(runtimeSwitchMsgForTest(
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			presetPrimary,
			"",
			stubBackend{
				sess:     &stubSession{events: make(chan session.AgentEvent)},
				provider: "openai",
				model:    "gpt-4.1",
			},
			&stubSession{events: make(chan session.AgentEvent)},
			storageSess,
		))
		returned <- updateResult{model: testModel(t, updated), cmd: cmd}
	}()

	var result updateResult
	select {
	case result = <-returned:
	case <-time.After(2 * time.Second):
		t.Fatal("runtime switch blocked on usage load")
	}
	if result.cmd == nil {
		t.Fatal("expected runtime switch command")
	}
	select {
	case <-storageSess.started:
		t.Fatal("usage load ran inside Update")
	default:
	}

	firstCmd := firstSequenceCmd(t, result.cmd)
	loaded := make(chan tea.Msg, 1)
	go func() {
		loaded <- firstCmd()
	}()
	select {
	case <-storageSess.started:
	case <-time.After(2 * time.Second):
		t.Fatal("runtime switch command did not load usage")
	}
	select {
	case msg := <-loaded:
		t.Fatalf("usage command returned before storage completed: %T", msg)
	default:
	}

	close(storageSess.release)
	msg := <-loaded
	usage, ok := msg.(sessionUsageLoadedMsg)
	if !ok {
		t.Fatalf("usage command result = %T, want sessionUsageLoadedMsg", msg)
	}
	updated, _ := result.model.Update(usage)
	result.model = testModel(t, updated)
	if result.model.Progress.TokensSent != 1200 || result.model.Progress.TokensReceived != 300 {
		t.Fatalf(
			"usage = %d/%d, want 1200/300",
			result.model.Progress.TokensSent,
			result.model.Progress.TokensReceived,
		)
	}
}

func firstSequenceCmd(t *testing.T, cmd tea.Cmd) tea.Cmd {
	t.Helper()
	msg := cmd()
	value := reflect.ValueOf(msg)
	cmdType := reflect.TypeOf(tea.Cmd(nil))
	if value.Kind() != reflect.Slice || value.Type().Elem() != cmdType || value.Len() == 0 {
		t.Fatalf("command message = %T, want non-empty sequence", msg)
	}
	first, ok := value.Index(0).Interface().(tea.Cmd)
	if !ok {
		t.Fatalf("sequence element = %T, want tea.Cmd", value.Index(0).Interface())
	}
	return first
}

func TestRuntimeSwitchSnapshotTracksLazySessionWithoutResumingIt(t *testing.T) {
	model := readyModel(t)
	lazy := session.NewLazySession(&resumeOnlyStore{}, "/tmp/test", "openai/gpt-4.1", "main")

	updated, _ := model.Update(runtimeSwitchMsgForTest(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		presetPrimary,
		"ready",
		stubBackend{
			sess:     &stubSession{events: make(chan session.AgentEvent)},
			provider: "openai",
			model:    "gpt-4.1",
		},
		&stubSession{events: make(chan session.AgentEvent)},
		lazy,
	))
	model = testModel(t, updated)

	if model.Model.Runtime.SessionID == "" {
		t.Fatal("runtime snapshot session id is empty, want lazy session identity tracked")
	}
	if model.Model.Runtime.Materialized {
		t.Fatal("runtime snapshot marked lazy session as materialized")
	}
	if got := model.ResumeSessionID(); got != "" {
		t.Fatalf("resume session id = %q, want empty for unmaterialized accepted runtime", got)
	}
}

func TestRuntimeTransitionCommitPreservesAcceptedSessionSnapshot(t *testing.T) {
	model := readyModel(t)
	model.Model.Storage = &stubStorageSession{
		id:     "session-1",
		model:  "openai/gpt-4.1",
		branch: "main",
	}

	var err error
	model, err = model.commitRuntimeTransition(newRuntimeTransition(
		&config.Config{Provider: "openai", Model: "gpt-4.1", ReasoningEffort: "high"},
		&config.Config{Provider: "openai", Model: "gpt-4.1", ReasoningEffort: "high"},
		presetPrimary,
		"",
	))
	if err != nil {
		t.Fatalf("commit runtime transition: %v", err)
	}

	if got := model.Model.Runtime.MaterializedSessionID(); got != "session-1" {
		t.Fatalf("snapshot session id = %q, want current accepted session", got)
	}
	if got := model.Progress.ReasoningEffort; got != "high" {
		t.Fatalf("reasoning = %q, want high", got)
	}
}

func TestRuntimeTransitionCommittedPreservesAcceptedSessionSnapshot(t *testing.T) {
	model := readyModel(t)
	model.Model.RuntimeSwitchRequest = 12
	model.Model.Storage = &stubStorageSession{
		id:     "session-1",
		model:  "openai/gpt-4.1",
		branch: "main",
	}
	model.Progress.Status = "Saving runtime settings..."

	updated, _ := model.Update(TransitionCommittedMsg{
		switchID: 12,
		transition: newRuntimeTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1", ReasoningEffort: "high"},
			&config.Config{Provider: "openai", Model: "gpt-4.1", ReasoningEffort: "high"},
			presetPrimary,
			"",
		),
		notice: session.Entry{Role: session.RoleSystem, Content: "Runtime changed"},
	})
	model = testModel(t, updated)

	if got := model.Model.Runtime.MaterializedSessionID(); got != "session-1" {
		t.Fatalf("snapshot session id = %q, want current accepted session", got)
	}
	if model.Model.RuntimeSwitchRequest != 0 {
		t.Fatalf("runtime switch request = %d, want cleared", model.Model.RuntimeSwitchRequest)
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

	oldSession := &stubSession{events: make(chan session.AgentEvent)}
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
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
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
	model = testModel(t, next)

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
		sess:     &stubSession{events: make(chan session.AgentEvent)},
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
	withOpenRouterKey(t)
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.AgentEvent)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]llm.ModelMetadata, error) {
			if cfg.Provider != "openrouter" {
				t.Fatalf("provider = %q, want openrouter", cfg.Provider)
			}
			return []llm.ModelMetadata{
				{ID: "z-ai/glm-4.5"},
				{ID: "z-ai/glm-5"},
				{ID: "z-ai/glm-5-turbo"},
			}, nil
		},
	)

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
	model = resolveProviderSelectionAndModelLoad(t, model, cmd)
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

func TestProviderPickerStagesListingProviderUntilModelSelection(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ANTHROPIC_API_KEY", "test-key")
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]llm.ModelMetadata, error) {
			if cfg.Provider != "anthropic" {
				t.Fatalf("provider = %q, want anthropic", cfg.Provider)
			}
			return []llm.ModelMetadata{{ID: "claude-test"}}, nil
		},
	)

	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.AgentEvent)},
		provider: "openai",
		model:    "gpt-4.1",
	}
	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    providerItems(&config.Config{}),
		filtered: providerItems(&config.Config{}),
		index:    pickerIndex(providerItems(&config.Config{}), "anthropic"),
		purpose:  pickerPurposeProvider,
		cfg:      &config.Config{Provider: "openai", Model: "gpt-4.1"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = resolveProviderSelectionAndModelLoad(t, updated, cmd)
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
	if got := model.Picker.Overlay.cfg.Provider; got != "anthropic" {
		t.Fatalf("picker provider = %q, want anthropic", got)
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "state.toml")); !os.IsNotExist(err) {
		t.Fatalf("state file error = %v, want provider unstored until model selection", err)
	}
	if got := model.Model.Backend.Provider(); got != "openai" {
		t.Fatalf("backend provider = %q, want unchanged openai", got)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model = %q, want unchanged gpt-4.1", got)
	}
}

func TestModelPickerRejectsProviderWithoutModelListing(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.openModelPickerWithConfig(&config.Config{Provider: "zai"})
	model = updated
	if model.Picker.Overlay != nil {
		t.Fatal("expected no model picker for provider without listing support")
	}
	if cmd == nil {
		t.Fatal("expected model picker error command")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "Set a model with /model <id>") {
		t.Fatalf("error = %v, want manual model entry notice", err)
	}
}

func TestProviderPickerSelectingNonListingProviderClearsStaleError(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ZAI_API_KEY", "test-key")
	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "failed to list models for zai"
	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    providerItems(&config.Config{}),
		filtered: providerItems(&config.Config{}),
		index:    pickerIndex(providerItems(&config.Config{}), "zai"),
		purpose:  pickerPurposeProvider,
		cfg:      &config.Config{Provider: "openrouter", Model: "vendor/model-b"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model, cmd = resolveProviderSelection(t, updated, cmd)
	if cmd == nil {
		t.Fatal("expected non-listing provider selection notice")
	}
	model, cmd = settleRuntimeTransitionCmd(t, model, cmd)
	if cmd == nil {
		t.Fatal("expected non-listing provider selection print command")
	}
	if model.Progress.Mode == stateError || model.Progress.LastError != "" {
		t.Fatalf(
			"stale error not cleared: mode=%v err=%q",
			model.Progress.Mode,
			model.Progress.LastError,
		)
	}
	cfg, err := config.Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "zai" {
		t.Fatalf("config provider = %q, want zai", cfg.Provider)
	}
	if cfg.Model != "" {
		t.Fatalf("config model = %q, want cleared model", cfg.Model)
	}
}

func TestProviderCommandClearsStaleError(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ANTHROPIC_API_KEY", "test-key")
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]llm.ModelMetadata, error) {
			if cfg.Provider != "anthropic" {
				t.Fatalf("provider = %q, want anthropic", cfg.Provider)
			}
			return []llm.ModelMetadata{{ID: "claude-test"}}, nil
		},
	)

	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "failed to list models for zai"

	updated, cmd := model.handleCommand("/provider anthropic")
	model = updated

	model = resolveProviderSelectionAndModelLoad(t, model, cmd)
	if model.Progress.Mode == stateError || model.Progress.LastError != "" {
		t.Fatalf(
			"stale error not cleared: mode=%v err=%q",
			model.Progress.Mode,
			model.Progress.LastError,
		)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
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

	oldSession := &stubSession{events: make(chan session.AgentEvent)}
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
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			resolved := *cfg
			newBackend := testutil.New()
			newBackend.SetConfig(&resolved)
			newBackend.SetSession(newStorage)
			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	msg := runtimeSwitchMsgForTest(
		nil,
		nil,
		"",
		"ready",
		testutil.New(),
		testutil.New(),
		newStorage,
	)
	msg.notice = "Switched model to gpt-4.1"
	next, _ := model.Update(msg)
	model = testModel(t, next)

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
	model.Progress.LastTurnSummary = core.TurnSummary{Elapsed: time.Second, Input: 1, Output: 2, Cost: 3}

	next, _ := model.Update(runtimeSwitchMsgForTest(
		nil,
		nil,
		"",
		"ready",
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		&stubSession{events: make(chan session.AgentEvent)},
		&stubStorageSession{id: "session-1", branch: "main"},
	))
	model = testModel(t, next)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want cleared on runtime switch", model.InFlight.QueuedTurns)
	}
	if model.Progress.LastError != "" {
		t.Fatalf("last error = %q, want cleared on runtime switch", model.Progress.LastError)
	}
	if model.Progress.LastTurnSummary != (core.TurnSummary{}) {
		t.Fatalf(
			"last turn summary = %#v, want cleared on runtime switch",
			model.Progress.LastTurnSummary,
		)
	}
}

func TestRuntimeSwitchIgnoresStaleAwaitedSessionEvents(t *testing.T) {
	oldSession := &stubSession{events: make(chan session.AgentEvent, 1)}
	newSession := &stubSession{events: make(chan session.AgentEvent, 1)}
	model := readyModel(t)
	model.Model.Session = oldSession
	waitOld := model.awaitSessionEvent()

	next, _ := model.Update(runtimeSwitchMsgForTest(
		nil,
		nil,
		"",
		"ready",
		stubBackend{sess: newSession},
		newSession,
		&stubStorageSession{id: "session-1", branch: "main"},
	))
	model = testModel(t, next)

	oldSession.events <- session.NewTextUpdate("stale output", session.AgentMessage{})
	next, cmd := model.Update(waitOld())
	model = testModel(t, next)

	if cmd != nil {
		t.Fatalf("stale session event scheduled command %T", cmd)
	}
	if model.InFlight.Pending != nil || model.InFlight.StreamBuf != "" {
		t.Fatalf(
			"stale event affected stream state: pending=%#v stream=%q",
			model.InFlight.Pending,
			model.InFlight.StreamBuf,
		)
	}
	if model.Progress.Mode != stateReady {
		t.Fatalf("mode = %v, want ready after stale event", model.Progress.Mode)
	}

	newSession.events <- session.TurnStart{}
	next, _ = model.Update(model.awaitSessionEvent()())
	model = testModel(t, next)

	if !model.InFlight.Thinking {
		t.Fatal("current session event was not accepted")
	}
	if model.Progress.Mode != stateIonizing {
		t.Fatalf("mode = %v, want ionizing after current event", model.Progress.Mode)
	}
}

func TestRuntimeSwitchIgnoresStaleCompletion(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	initialSession := &stubSession{events: make(chan session.AgentEvent)}
	type openedRuntime struct {
		session *stubSession
		storage *stubStorageSession
	}
	opened := make(map[string]openedRuntime)

	model := New(
		stubBackend{sess: initialSession, provider: "openai", model: "gpt-4.1"},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			sess := &stubSession{events: make(chan session.AgentEvent)}
			storageSess := &stubStorageSession{id: cfg.Model, model: cfg.Provider + "/" + cfg.Model}
			opened[cfg.Model] = openedRuntime{session: sess, storage: storageSess}
			return stubBackend{
				sess:     sess,
				provider: cfg.Provider,
				model:    cfg.Model,
			}, sess, storageSess, nil
		},
	)

	var firstCmd tea.Cmd
	model, firstCmd = model.switchRuntimeCommand(
		newRuntimeTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1-first"},
			&config.Config{Provider: "openai", Model: "gpt-4.1-first"},
			presetPrimary,
			"",
		),
		session.Entry{Role: session.RoleSystem, Content: "First"},
		"",
		false,
	)
	var secondCmd tea.Cmd
	model, secondCmd = model.switchRuntimeCommand(
		newRuntimeTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1-second"},
			&config.Config{Provider: "openai", Model: "gpt-4.1-second"},
			presetPrimary,
			"",
		),
		session.Entry{Role: session.RoleSystem, Content: "Second"},
		"",
		false,
	)

	next, cmd := model.Update(firstCmd())
	model = testModel(t, next)
	if cmd != nil {
		t.Fatalf("stale runtime switch returned command %T", cmd)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model after stale completion = %q, want original", got)
	}
	if initialSession.closed {
		t.Fatal("stale runtime switch closed the active old session")
	}
	stale := opened["gpt-4.1-first"]
	if stale.session == nil || !stale.session.closed {
		t.Fatal("stale switched session was not closed")
	}
	if stale.storage == nil || !stale.storage.closed {
		t.Fatal("stale switched storage was not closed")
	}

	next, cmd = model.Update(secondCmd())
	model = testModel(t, next)
	if cmd == nil {
		t.Fatal("current runtime switch should schedule replay/await commands")
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1-second" {
		t.Fatalf("backend model = %q, want second switch", got)
	}
	if !initialSession.closed {
		t.Fatal("accepted runtime switch did not close the old session")
	}
	current := opened["gpt-4.1-second"]
	if current.session == nil || current.session.closed {
		t.Fatal("current switched session was closed or missing")
	}
	if current.storage == nil || current.storage.closed {
		t.Fatal("current switched storage was closed or missing")
	}
	if model.Model.RuntimeSwitchRequest != 0 {
		t.Fatalf("runtime switch request = %d, want cleared", model.Model.RuntimeSwitchRequest)
	}
}

func TestRuntimeSwitchClosesPreviousStorageSession(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	oldSession := &stubSession{events: make(chan session.AgentEvent)}
	oldStorage := &stubStorageSession{id: "old-session", model: "openai/old", branch: "main"}
	newSession := &stubSession{events: make(chan session.AgentEvent)}
	newStorage := &stubStorageSession{
		id:     "new-session",
		model:  "openai/new",
		branch: "feature/switch",
	}

	model := New(
		stubBackend{sess: oldSession, provider: "openai", model: "old"},
		oldStorage,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return stubBackend{
				sess:     newSession,
				provider: cfg.Provider,
				model:    cfg.Model,
			}, newSession, newStorage, nil
		},
	)

	model, cmd := model.switchRuntimeCommand(
		newRuntimeTransition(
			&config.Config{Provider: "openai", Model: "new"},
			&config.Config{Provider: "openai", Model: "new"},
			presetPrimary,
			"",
		),
		session.Entry{Role: session.RoleSystem, Content: "Switched"},
		oldStorage.ID(),
		false,
	)
	if cmd == nil {
		t.Fatal("expected runtime switch command")
	}
	if model.Progress.LocalStatus != "Switching runtime..." {
		t.Fatalf("local status = %q, want switching status", model.Progress.LocalStatus)
	}
	rawMsg := cmd()
	msg, ok := rawMsg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("switch command message = %T, want runtimeSwitchedMsg", rawMsg)
	}

	next, _ := model.Update(msg)
	model = testModel(t, next)

	if !oldSession.closed {
		t.Fatal("old agent session was not closed")
	}
	if !oldStorage.closed {
		t.Fatal("old storage session was not closed")
	}
	if newSession.closed {
		t.Fatal("new agent session was closed")
	}
	if newStorage.closed {
		t.Fatal("new storage session was closed")
	}
	if model.Model.Storage != newStorage {
		t.Fatalf("active storage = %#v, want new storage", model.Model.Storage)
	}
}

func TestRuntimeSwitchErrorClearsSwitchingStatus(t *testing.T) {
	model := readyModel(t)
	model.Model.RuntimeSwitchRequest = 7
	model.Progress.Status = "Switching runtime..."

	next, cmd := model.Update(runtimeSwitchErrorMsg{
		switchID: 7,
		err:      errors.New("Local API is not running"),
	})
	model = testModel(t, next)

	if cmd == nil {
		t.Fatal("expected local error print command")
	}
	if model.Model.RuntimeSwitchRequest != 0 {
		t.Fatalf("runtime switch request = %d, want cleared", model.Model.RuntimeSwitchRequest)
	}
	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared after switch error", model.Progress.Status)
	}
	if line := ansi.Strip(model.progressLine()); strings.Contains(line, "Switching runtime") {
		t.Fatalf("progress line = %q, want no stale switching status", line)
	}
}

func TestResumeRuntimeSwitchClosesPreviousStorageSession(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	oldSession := &stubSession{events: make(chan session.AgentEvent)}
	oldStorage := &stubStorageSession{id: "old-session", model: "openai/old", branch: "main"}
	newSession := &stubSession{events: make(chan session.AgentEvent)}
	newStorage := &stubStorageSession{
		id:     "resumed-session",
		model:  "openai/new",
		branch: "feature/resume",
	}

	model := New(
		stubBackend{sess: oldSession, provider: "openai", model: "old"},
		oldStorage,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return stubBackend{
				sess:     newSession,
				provider: cfg.Provider,
				model:    cfg.Model,
			}, newSession, newStorage, nil
		},
	)

	model, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "new"},
		session.Entry{Role: session.RoleSystem, Content: "Resumed"},
		newStorage.ID(),
	)
	if cmd == nil {
		t.Fatal("expected resume runtime command")
	}
	rawMsg := cmd()
	msg, ok := rawMsg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("resume command message = %T, want runtimeSwitchedMsg", rawMsg)
	}

	next, _ := model.Update(msg)
	model = testModel(t, next)

	if !oldSession.closed {
		t.Fatal("old agent session was not closed")
	}
	if !oldStorage.closed {
		t.Fatal("old storage session was not closed")
	}
	if newSession.closed {
		t.Fatal("new agent session was closed")
	}
	if newStorage.closed {
		t.Fatal("new storage session was closed")
	}
	if model.Model.Storage != newStorage {
		t.Fatalf("active storage = %#v, want resumed storage", model.Model.Storage)
	}
}

func TestResumeRuntimeSwitchPersistsPrimaryPreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := config.SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.AgentEvent)}
	newSession := &stubSession{events: make(chan session.AgentEvent)}
	newStorage := &stubStorageSession{
		id:     "resumed-session",
		model:  "openai/new",
		branch: "feature/resume",
	}
	model := New(
		stubBackend{sess: oldSession, provider: "openai", model: "old"},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return stubBackend{
				sess:     newSession,
				provider: cfg.Provider,
				model:    cfg.Model,
			}, newSession, newStorage, nil
		},
	).WithActivePreset("fast")

	model, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "new"},
		session.Entry{Role: session.RoleSystem, Content: "Resumed"},
		newStorage.ID(),
	)
	if cmd == nil {
		t.Fatal("expected resume runtime command")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("resume command message = %T, want runtimeSwitchedMsg", msg)
	}
	next, _ := model.Update(switched)
	model = testModel(t, next)

	if model.App.ActivePreset != presetPrimary {
		t.Fatalf("active preset = %q, want primary", model.App.ActivePreset)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "primary" {
		t.Fatalf("state active_preset = %#v, want primary", state.ActivePreset)
	}
}

func TestResumeRuntimeWithoutSwitcherUpdatesAppConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	capture := &configCaptureBackend{
		stubBackend: stubBackend{
			sess:     &stubSession{events: make(chan session.AgentEvent)},
			provider: "openai",
			model:    "old",
		},
	}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil)

	updated, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openrouter", Model: "z-ai/glm-5"},
		session.Entry{Role: session.RoleSystem, Content: "Resumed"},
		"session-1",
	)
	model = updated
	if cmd == nil {
		t.Fatal("expected resume notice")
	}
	model, cmd = settleRuntimeTransitionCmd(t, model, cmd)
	if cmd == nil {
		t.Fatal("expected resume print command")
	}
	if capture.cfg == nil ||
		capture.cfg.Provider != "openrouter" ||
		capture.cfg.Model != "z-ai/glm-5" {
		t.Fatalf("backend config = %#v, want resumed runtime", capture.cfg)
	}
	if model.Model.Config == nil ||
		model.Model.Config.Provider != "openrouter" ||
		model.Model.Config.Model != "z-ai/glm-5" {
		t.Fatalf("app config = %#v, want resumed runtime", model.Model.Config)
	}
}

func TestRuntimeSwitchClosesNewRuntimeWhenStateSaveFails(t *testing.T) {
	t.Setenv("HOME", "/dev/null")
	oldSession := &stubSession{events: make(chan session.AgentEvent)}
	newSession := &stubSession{events: make(chan session.AgentEvent)}
	newStorage := &stubStorageSession{id: "new-session", branch: "main"}
	model := New(
		stubBackend{sess: oldSession},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	model, cmd := model.switchRuntimeCommand(
		newRuntimeTransition(
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			&config.Config{Provider: "openai", Model: "gpt-4.1"},
			presetFast,
			"",
		),
		session.Entry{Role: session.RoleSystem, Content: "Switched"},
		"",
		false,
	)
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "save state") {
		t.Fatalf("switch error = %v, want save state error", err)
	}
	if oldSession.closed {
		t.Fatal("old session was closed after failed switch")
	}
	if !newSession.closed {
		t.Fatal("new session was not closed after failed switch")
	}
	if !newStorage.closed {
		t.Fatal("new storage was not closed after failed switch")
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
		sess:     &stubSession{events: make(chan session.AgentEvent)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}

	model, cmd := model.handleCommand("/model z-ai/glm-5")
	if cmd != nil {
		t.Fatalf("expected no-op command for same model, got %T", cmd)
	}
}

func TestSlashModelUsesRuntimeConfigOverPersistedState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(cfgDir, "state.toml"),
		[]byte("provider = \"local-api\"\nmodel = \"qwen3.6:27b\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.AgentEvent)}
	var observed *config.Config
	model := New(
		stubBackend{sess: oldSession, provider: "openrouter", model: "tencent/hy3-preview:free"},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			copied := *cfg
			observed = &copied
			newBackend := testutil.New()
			newBackend.SetConfig(&copied)
			return newBackend, newBackend.Session(), nil, nil
		},
	).WithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "tencent/hy3-preview:free",
	})

	model, cmd := model.handleCommand("/model anthropic/claude-sonnet-4.5")
	if cmd == nil {
		t.Fatal("expected runtime switch command")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ := model.Update(switched)
	model = testModel(t, next)

	if observed == nil || observed.Provider != "openrouter" ||
		observed.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("switcher config = %#v, want active openrouter provider", observed)
	}
	if model.Model.Config == nil ||
		model.Model.Config.Provider != "openrouter" ||
		model.Model.Config.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("app config = %#v, want updated openrouter model", model.Model.Config)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.Provider == nil || *state.Provider != "openrouter" ||
		state.Model == nil || *state.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("state = %#v, want explicit slash command selection persisted", state)
	}
}

func TestRuntimeSwitchShowsStatusOnResume(t *testing.T) {
	model := readyModel(t)
	model.Model.Session = &stubSession{events: make(chan session.AgentEvent)}

	msg := runtimeSwitchMsgForTest(
		nil,
		nil,
		"",
		"Connected via Canto",
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		&stubSession{events: make(chan session.AgentEvent)},
		&stubStorageSession{id: "session-1", branch: "main"},
	)
	msg.printLines = []string{"ion v0.0.0", "~/tmp/test • main", "", "--- resumed ---"}
	msg.replayEntries = []session.Entry{{Role: session.RoleUser, Content: "hello"}}
	msg.notice = "Resumed session session-1"
	updated, cmd := model.Update(msg)
	model = testModel(t, updated)

	if model.Progress.Status != "Connected via Canto" {
		t.Fatalf("status = %q", model.Progress.Status)
	}
	if cmd == nil {
		t.Fatal("expected command batch for runtime switch")
	}
}

func TestRuntimeSwitchMarksPrintedTranscriptForReplay(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = false
	model.Model.Session = &stubSession{events: make(chan session.AgentEvent)}

	msg := runtimeSwitchMsgForTest(
		nil,
		nil,
		"",
		"ready",
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		&stubSession{events: make(chan session.AgentEvent)},
		&stubStorageSession{id: "session-1", branch: "main"},
	)
	msg.printLines = []string{"ion v0.0.0", "--- resumed ---"}
	msg.replayEntries = []session.Entry{{Role: session.RoleAgent, Content: "restored answer"}}
	updated, _ := model.Update(msg)
	model = testModel(t, updated)

	if !model.App.PrintedTranscript {
		t.Fatal("runtime replay did not mark transcript as printed")
	}
	if progress := ansi.Strip(model.progressLine()); strings.Contains(progress, "Ready") {
		t.Fatalf("progress line = %q, want idle ready suppressed after replay", progress)
	}
}

func TestRuntimeSwitchMarksPrintedTranscriptForHeaderOnlyReplay(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = false
	model.Model.Session = &stubSession{events: make(chan session.AgentEvent)}

	msg := runtimeSwitchMsgForTest(
		nil,
		nil,
		"",
		"ready",
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		&stubSession{events: make(chan session.AgentEvent)},
		&stubStorageSession{id: "session-1", branch: "main"},
	)
	msg.printLines = []string{"ion v0.0.0", "--- resumed ---"}
	updated, _ := model.Update(msg)
	model = testModel(t, updated)

	if !model.App.PrintedTranscript {
		t.Fatal("runtime replay header did not mark transcript as printed")
	}
	if progress := ansi.Strip(model.progressLine()); strings.Contains(progress, "Ready") {
		t.Fatalf("progress line = %q, want idle ready suppressed after replay header", progress)
	}
}

func TestRuntimeSwitchMarksPrintedTranscriptForNotice(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = false
	model.Model.Session = &stubSession{events: make(chan session.AgentEvent)}

	msg := runtimeSwitchMsgForTest(
		nil,
		nil,
		"",
		"ready",
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		&stubSession{events: make(chan session.AgentEvent)},
		&stubStorageSession{id: "session-1", branch: "main"},
	)
	msg.notice = "Switched to fast"
	updated, _ := model.Update(msg)
	model = testModel(t, updated)

	if !model.App.PrintedTranscript {
		t.Fatal("runtime switch notice did not mark transcript as printed")
	}
	if progress := ansi.Strip(model.progressLine()); strings.Contains(progress, "Ready") {
		t.Fatalf("progress line = %q, want idle ready suppressed after notice", progress)
	}
}

func runResumeStoredSessionCommand(t *testing.T, model Model, sessionID string) (Model, tea.Msg) {
	t.Helper()

	updated, cmd := model.resumeStoredSessionByID(sessionID)
	if cmd == nil {
		t.Fatal("resumeStoredSessionByID returned nil command")
	}
	first := cmd()
	selected, ok := first.(resumeSessionSelectedMsg)
	if !ok {
		t.Fatalf("expected resumeSessionSelectedMsg, got %T", first)
	}
	nextModel, nextCmd := updated.Update(selected)
	updated = testModel(t, nextModel)
	if nextCmd == nil {
		t.Fatal("resumeSessionSelectedMsg returned nil runtime switch command")
	}
	return updated, nextCmd()
}

type blockingResumeStore struct {
	resumeOnlyStore
	started chan struct{}
	release chan struct{}
	session session.SessionHandle
}

func (s *blockingResumeStore) ResumeSession(
	ctx context.Context,
	id string,
) (session.SessionHandle, error) {
	close(s.started)
	select {
	case <-s.release:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	return s.session, nil
}

func TestResumeStoredSessionReturnsBeforeInspectionCompletes(t *testing.T) {
	store := &blockingResumeStore{
		started: make(chan struct{}),
		release: make(chan struct{}),
		session: &stubStorageSession{
			id:    "session-1",
			model: "openai/gpt-4.1",
		},
	}
	model := readyModel(t)
	model.Model.Store = store
	model.Model.Config = &config.Config{Provider: "openai", Model: "gpt-4.1"}

	updated, cmd := model.resumeStoredSessionByID("session-1")
	if cmd == nil {
		t.Fatal("resumeStoredSessionByID returned nil command")
	}
	if updated.Model.RuntimeSwitchRequest == 0 {
		t.Fatal("resumeStoredSessionByID did not mark runtime switch in progress")
	}
	select {
	case <-store.started:
		t.Fatal("ResumeSession ran before Bubble Tea command execution")
	default:
	}

	loaded := make(chan tea.Msg, 1)
	go func() {
		loaded <- cmd()
	}()
	select {
	case <-store.started:
	case <-time.After(2 * time.Second):
		t.Fatal("resume command did not inspect stored session")
	}
	select {
	case msg := <-loaded:
		t.Fatalf("resume command returned before inspection completed: %T", msg)
	default:
	}

	close(store.release)
	msg := <-loaded
	if _, ok := msg.(resumeSessionSelectedMsg); !ok {
		t.Fatalf("resume command result = %T, want resumeSessionSelectedMsg", msg)
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
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		nil,
		&resumeOnlyStore{resumed: tempSession},
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
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

	_, msg := runResumeStoredSessionCommand(t, model, "session-1")

	if _, ok := msg.(runtimeSwitchedMsg); !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	if !tempSession.closed {
		t.Fatal("expected temporary inspection session to be closed after reading metadata")
	}
}

func TestResumeStoredSessionPreservesOpenAICompatibleEndpoint(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	const endpoint = "http://fedora:8080/v1"
	if err := os.WriteFile(
		filepath.Join(cfgDir, "config.toml"),
		[]byte(
			"provider = \"openai-compatible\"\n"+
				"model = \"old-model\"\n"+
				"endpoint = \""+endpoint+"\"\n",
		),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}

	tempSession := &stubStorageSession{
		id:     "session-1",
		model:  "openai-compatible/qwen3.6:27b",
		branch: "main",
	}

	var captured config.Config
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		nil,
		&resumeOnlyStore{resumed: tempSession},
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			captured = *cfg
			newSession := &stubSession{events: make(chan session.AgentEvent)}
			opened := &stubStorageSession{
				id:     sessionID,
				model:  cfg.Provider + "/" + cfg.Model,
				branch: "feature/resume",
			}
			return stubBackend{
				sess:     newSession,
				provider: cfg.Provider,
				model:    cfg.Model,
			}, newSession, opened, nil
		},
	)

	_, msg := runResumeStoredSessionCommand(t, model, "session-1")

	if _, ok := msg.(runtimeSwitchedMsg); !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	if captured.Provider != "openai-compatible" ||
		captured.Model != "qwen3.6:27b" ||
		captured.Endpoint != endpoint {
		t.Fatalf(
			"resume config = %#v, want openai-compatible qwen with endpoint %q",
			captured,
			endpoint,
		)
	}
}

func TestConfigForStoredSessionClearsProviderScopedPresets(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	model := readyModel(t)
	model.Model.Config = &config.Config{
		Provider:               "local-api",
		Model:                  "qwen3.6:27b",
		ReasoningEffort:        "high",
		FastModel:              "qwen3.6:27b-fast",
		FastReasoningEffort:    "low",
		SummaryModel:           "qwen3.6:27b-summary",
		SummaryReasoningEffort: "minimal",
	}

	cfg, err := model.configForStoredSession("openrouter", "openai/gpt-5.4")
	if err != nil {
		t.Fatalf("config for stored session: %v", err)
	}
	if cfg.Provider != "openrouter" || cfg.Model != "openai/gpt-5.4" {
		t.Fatalf(
			"cfg provider/model = %s/%s, want openrouter/openai/gpt-5.4",
			cfg.Provider,
			cfg.Model,
		)
	}
	if cfg.ReasoningEffort != "high" {
		t.Fatalf("reasoning effort = %q, want high", cfg.ReasoningEffort)
	}
	if cfg.FastModel != "" ||
		cfg.FastReasoningEffort != "" ||
		cfg.SummaryModel != "" ||
		cfg.SummaryReasoningEffort != "" {
		t.Fatalf("provider-scoped presets were not cleared: %#v", cfg)
	}
}

func TestResumeRuntimeCommandPrintsMarkerAfterHeader(t *testing.T) {
	newSession := &stubSession{events: make(chan session.AgentEvent)}
	newStorage := &stubStorageSession{
		id:      "session-1",
		model:   "openai/gpt-4.1",
		branch:  "feature/resume",
		entries: []session.Entry{{Role: session.RoleUser, Content: "hello"}},
	}
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	model, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		session.Entry{Role: session.RoleSystem, Content: "Resumed"},
		"session-1",
	)
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}

	got := make([]string, 0, len(switched.printLines))
	for _, line := range switched.printLines {
		got = append(got, ansi.Strip(line))
	}
	want := []string{"ion dev", "/tmp/test • feature/resume", "", "--- resumed ---", ""}
	if !slices.Equal(got, want) {
		t.Fatalf("print lines = %#v, want %#v", got, want)
	}
	if len(switched.replayEntries) != 1 || switched.replayEntries[0].Content != "hello" {
		t.Fatalf("replay entries = %#v", switched.replayEntries)
	}
}

func TestResumeRuntimeCommandClosesNewRuntimeWhenReplayFails(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := config.SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.AgentEvent)}
	newSession := &stubSession{events: make(chan session.AgentEvent)}
	newStorage := &stubStorageSession{
		id:         "session-1",
		model:      "openai/gpt-4.1",
		branch:     "main",
		entriesErr: errors.New("bad replay"),
	}
	model := New(
		stubBackend{sess: oldSession},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (core.Backend, session.AgentSession, session.SessionHandle, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	model, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		session.Entry{Role: session.RoleSystem, Content: "Resumed"},
		"session-1",
	)
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(
		err.Error(),
		"load session transcript",
	) {
		t.Fatalf("resume error = %v, want transcript load error", err)
	}
	if oldSession.closed {
		t.Fatal("old session was closed after failed resume")
	}
	if !newSession.closed {
		t.Fatal("new session was not closed after failed resume")
	}
	if !newStorage.closed {
		t.Fatal("new storage was not closed after failed resume")
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("state active_preset = %#v, want fast after failed resume", state.ActivePreset)
	}
}

func TestProgressLineShowsConfigurationWarning(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.AgentEvent)},
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
		sess:        &stubSession{events: make(chan session.AgentEvent)},
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
		sess:        &stubSession{events: make(chan session.AgentEvent)},
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
		sess:        &stubSession{events: make(chan session.AgentEvent)},
		provider:    "openrouter",
		providerSet: true,
		model:       "z-ai/glm-5",
		modelSet:    true,
	}
	model.Progress.Status = "Connected via Ion"

	line := ansi.Strip(model.progressLine())
	if strings.Contains(line, "Connected via Ion") {
		t.Fatalf("progress line should suppress bootstrap connection notice: %q", line)
	}
	if !strings.Contains(line, "Ready") {
		t.Fatalf("progress line = %q, want Ready", line)
	}
}
