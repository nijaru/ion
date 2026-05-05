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
	surface      backend.ToolSurface
}

type compactBackend struct {
	stubBackend
	compacted bool
	err       error
	called    bool
}

type configCaptureBackend struct {
	stubBackend
	cfg *config.Config
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

func (b stubBackend) ToolSurface() backend.ToolSurface {
	if b.surface.Count != 0 ||
		b.surface.Sandbox != "" ||
		b.surface.Environment != "" ||
		len(b.surface.Names) > 0 {
		return b.surface
	}
	return backend.ToolSurface{
		Count:         2,
		LazyThreshold: 20,
		Names:         []string{"read", "write"},
	}
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

func (b *configCaptureBackend) SetConfig(cfg *config.Config) {
	if cfg == nil {
		b.cfg = nil
		return
	}
	copied := *cfg
	b.cfg = &copied
}

func (b *compactBackend) Compact(ctx context.Context) (bool, error) {
	b.called = true
	return b.compacted, b.err
}

type stubSession struct {
	events      chan session.Event
	submits     []string
	cancels     int
	submitErr   error
	approveErr  error
	approvals   []stubApproval
	allowed     []string
	mode        session.Mode
	autoApprove bool
	closed      bool
}

type steeringStubSession struct {
	stubSession
	steers []string
	result session.SteeringResult
	err    error
}

type stubApproval struct {
	id string
	ok bool
}

func localErrorFromMsg(t *testing.T, msg tea.Msg) error {
	t.Helper()
	errMsg, ok := msg.(localErrorMsg)
	if !ok {
		t.Fatalf("message = %T, want localErrorMsg", msg)
	}
	return errMsg.err
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
	s.closed = true
	if s.events != nil {
		close(s.events)
		s.events = nil
	}
	return nil
}
func (s *stubSession) Events() <-chan session.Event { return s.events }
func (s *stubSession) Approve(ctx context.Context, id string, ok bool) error {
	s.approvals = append(s.approvals, stubApproval{id: id, ok: ok})
	return s.approveErr
}

func (s *stubSession) SetMode(mode session.Mode) { s.mode = mode }

func (s *stubSession) SetAutoApprove(enabled bool) { s.autoApprove = enabled }
func (s *stubSession) AllowCategory(category string) {
	s.allowed = append(s.allowed, category)
}
func (s *stubSession) ID() string              { return "stub" }
func (s *stubSession) Meta() map[string]string { return nil }

func (s *steeringStubSession) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	s.steers = append(s.steers, text)
	if s.err != nil {
		return session.SteeringResult{}, s.err
	}
	if s.result.Outcome == "" {
		return session.SteeringResult{Outcome: session.SteeringAccepted}, nil
	}
	return s.result, nil
}

type stubStorageSession struct {
	id         string
	model      string
	branch     string
	closed     bool
	appends    []any
	appendErr  error
	usageIn    int
	usageOut   int
	usageCost  float64
	entries    []session.Entry
	entriesErr error
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
	if s.entriesErr != nil {
		return nil, s.entriesErr
	}
	return append([]session.Entry(nil), s.entries...), nil
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

type forkTreeStore struct {
	resumeOnlyStore
	forked     storage.Session
	forkParent string
	forkOpts   storage.ForkOptions
	tree       storage.SessionTree
}

func (s *forkTreeStore) ForkSession(
	ctx context.Context,
	parentID string,
	opts storage.ForkOptions,
) (storage.Session, error) {
	s.forkParent = parentID
	s.forkOpts = opts
	return s.forked, nil
}

func (s *forkTreeStore) SessionTree(
	ctx context.Context,
	sessionID string,
) (storage.SessionTree, error) {
	s.tree.Current.ID = sessionID
	return s.tree, nil
}

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

func TestWithConfigForRuntimeKeepsAppConfigAndAppliesRuntimeConfig(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	capture := &configCaptureBackend{stubBackend: stubBackend{sess: sess}}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil).
		WithConfigForRuntime(
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
		).
		WithActivePreset("fast")

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
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", cfg.Provider)
		}
		return []registry.ModelMetadata{
			{ID: "z-ai/glm-4.5"},
			{ID: "z-ai/glm-5"},
			{ID: "z-ai/glm-5-turbo"},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

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
	model = updated
	if cmd == nil {
		t.Fatal("expected non-listing provider selection notice")
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
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "anthropic" {
			t.Fatalf("provider = %q, want anthropic", cfg.Provider)
		}
		return []registry.ModelMetadata{{ID: "claude-test"}}, nil
	}
	t.Cleanup(func() { listModelsForConfig = oldListModelsForConfig })

	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "failed to list models for zai"

	updated, cmd := model.handleCommand("/provider anthropic")
	model = updated

	if cmd != nil {
		t.Fatalf("expected provider command to open picker without command, got %T", cmd)
	}
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
		t.Fatalf(
			"last turn summary = %#v, want cleared on runtime switch",
			model.Progress.LastTurnSummary,
		)
	}
}

func TestRuntimeSwitchClosesNewRuntimeWhenStateSaveFails(t *testing.T) {
	t.Setenv("HOME", "/dev/null")
	oldSession := &stubSession{events: make(chan session.Event)}
	newSession := &stubSession{events: make(chan session.Event)}
	newStorage := &stubStorageSession{id: "new-session", branch: "main"}
	model := New(
		stubBackend{sess: oldSession},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	cmd := model.switchRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		presetFast,
		session.Entry{Role: session.System, Content: "Switched"},
		"",
		false,
	)
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "save active preset") {
		t.Fatalf("switch error = %v, want save active preset error", err)
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
		sess:     &stubSession{events: make(chan session.Event)},
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

	oldSession := &stubSession{events: make(chan session.Event)}
	var observed *config.Config
	model := New(
		stubBackend{sess: oldSession, provider: "openrouter", model: "tencent/hy3-preview:free"},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
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
	model = next.(Model)

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
	model.Model.Session = &stubSession{events: make(chan session.Event)}

	updated, cmd := model.Update(runtimeSwitchedMsg{
		backend:       stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		session:       &stubSession{events: make(chan session.Event)},
		storage:       &stubStorageSession{id: "session-1", branch: "main"},
		printLines:    []string{"ion v0.0.0", "~/tmp/test • main", "", "--- resumed ---"},
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

func TestResumeRuntimeCommandPrintsMarkerAfterHeader(t *testing.T) {
	newSession := &stubSession{events: make(chan session.Event)}
	newStorage := &stubStorageSession{
		id:      "session-1",
		model:   "openai/gpt-4.1",
		branch:  "feature/resume",
		entries: []session.Entry{{Role: session.User, Content: "hello"}},
	}
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		session.Entry{Role: session.System, Content: "Resumed"},
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
	oldSession := &stubSession{events: make(chan session.Event)}
	newSession := &stubSession{events: make(chan session.Event)}
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
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		session.Entry{Role: session.System, Content: "Resumed"},
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
		"",
		model.renderEntry(session.Entry{Role: session.User, Content: "hello"}),
		"",
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
