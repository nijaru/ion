package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestScopedPatternCatalogRunsOutsideUpdateAndCancels(t *testing.T) {
	catalogStarted := make(chan struct{})
	catalogCanceled := make(chan struct{})
	catalog := modelCatalogStub{
		list: func(ctx context.Context, _ *config.Config) ([]llm.ModelMetadata, error) {
			close(catalogStarted)
			<-ctx.Done()
			close(catalogCanceled)
			return nil, ctx.Err()
		},
	}
	model := readyModelWithSwitcher(t, &[]string{}).WithModelCatalog(catalog).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Pattern: "openai/*"},
		},
	})

	type updateResult struct {
		model tea.Model
		cmd   tea.Cmd
	}
	updatedCh := make(chan updateResult, 1)
	go func() {
		updated, cmd := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
		updatedCh <- updateResult{model: updated, cmd: cmd}
	}()

	var result updateResult
	select {
	case result = <-updatedCh:
	case <-time.After(time.Second):
		t.Fatal("Ctrl+L blocked Update while expanding scoped models")
	}
	select {
	case <-catalogStarted:
		t.Fatal("catalog fetch started inside Update")
	default:
	}
	if result.cmd == nil {
		t.Fatal("Ctrl+L returned no scoped-model command")
	}
	updated := testModel(t, result.model)
	if updated.Model.RuntimeSwitchRequest == 0 {
		t.Fatal("scoped-model load did not own a runtime request")
	}

	messageCh := make(chan tea.Msg, 1)
	go func() { messageCh <- result.cmd() }()
	select {
	case <-catalogStarted:
	case <-time.After(time.Second):
		t.Fatal("scoped-model command did not start catalog fetch")
	}
	updated.Close()

	var message tea.Msg
	select {
	case message = <-messageCh:
	case <-time.After(time.Second):
		t.Fatal("scoped-model command did not observe runtime cancellation")
	}
	select {
	case <-catalogCanceled:
	case <-time.After(time.Second):
		t.Fatal("catalog fetch did not observe runtime cancellation")
	}
	loaded, ok := message.(scopedModelsLoadedMsg)
	if !ok || !errors.Is(loaded.err, context.Canceled) {
		t.Fatalf("scoped-model result = %#v, want canceled scopedModelsLoadedMsg", message)
	}
	settled, _ := updated.Update(loaded)
	if next := testModel(t, settled); next.Model.RuntimeSwitchRequest != 0 {
		t.Fatal("canceled scoped-model load left runtime request active")
	}
}

func TestScopedModelsCommandCatalogRunsOutsideUpdateAndCancels(t *testing.T) {
	catalogStarted := make(chan struct{})
	catalogCanceled := make(chan struct{})
	catalog := modelCatalogStub{
		list: func(ctx context.Context, _ *config.Config) ([]llm.ModelMetadata, error) {
			close(catalogStarted)
			<-ctx.Done()
			close(catalogCanceled)
			return nil, ctx.Err()
		},
	}
	model := readyModelWithSwitcher(t, &[]string{}).WithModelCatalog(catalog).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Pattern: "openai/*"},
		},
	})

	type commandResult struct {
		model Model
		cmd   tea.Cmd
	}
	resultCh := make(chan commandResult, 1)
	go func() {
		updated, cmd := model.handleCommand("/scoped-models")
		resultCh <- commandResult{model: updated, cmd: cmd}
	}()

	var result commandResult
	select {
	case result = <-resultCh:
	case <-time.After(time.Second):
		t.Fatal("/scoped-models blocked while expanding scoped models")
	}
	select {
	case <-catalogStarted:
		t.Fatal("catalog fetch started inside /scoped-models command handling")
	default:
	}
	if result.cmd == nil {
		t.Fatal("/scoped-models returned no scoped-model command")
	}
	updated := testModel(t, result.model)
	if updated.Model.RuntimeSwitchRequest == 0 {
		t.Fatal("scoped-model listing did not own a runtime request")
	}

	messageCh := make(chan tea.Msg, 1)
	go func() { messageCh <- result.cmd() }()
	select {
	case <-catalogStarted:
	case <-time.After(time.Second):
		t.Fatal("scoped-model command did not start catalog fetch")
	}
	updated.Close()

	var message tea.Msg
	select {
	case message = <-messageCh:
	case <-time.After(time.Second):
		t.Fatal("scoped-model command did not observe runtime cancellation")
	}
	select {
	case <-catalogCanceled:
	case <-time.After(time.Second):
		t.Fatal("catalog fetch did not observe runtime cancellation")
	}
	loaded, ok := message.(scopedModelsListedMsg)
	if !ok || !errors.Is(loaded.err, context.Canceled) {
		t.Fatalf("scoped-model listing result = %#v, want canceled scopedModelsListedMsg", message)
	}
	settled, _ := updated.Update(loaded)
	if next := testModel(t, settled); next.Model.RuntimeSwitchRequest != 0 {
		t.Fatal("canceled scoped-model listing left runtime request active")
	}
}

func TestScopedModelsCommandResultDisplaysResolvedModels(t *testing.T) {
	catalog := modelCatalogStub{
		list: func(context.Context, *config.Config) ([]llm.ModelMetadata, error) {
			return []llm.ModelMetadata{
				{Provider: "openai", ID: "gpt-4.1"},
				{Provider: "openai", ID: "gpt-4.1-mini"},
			}, nil
		},
	}
	model := readyModelWithSwitcher(t, &[]string{}).WithModelCatalog(catalog).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Pattern: "openai/*"},
		},
	})
	updatedValue, cmd := model.handleCommand("/scoped-models")
	if cmd == nil {
		t.Fatal("/scoped-models returned no scoped-model command")
	}
	message := cmd()
	loaded, ok := message.(scopedModelsListedMsg)
	if !ok {
		t.Fatalf("scoped-model listing result = %T, want scopedModelsListedMsg", message)
	}
	updated := testModel(t, updatedValue)
	next, renderCmd := updated.Update(loaded)
	if renderCmd == nil {
		t.Fatal("scoped-model listing returned no render command")
	}
	if next := testModel(t, next); next.Model.RuntimeSwitchRequest != 0 {
		t.Fatal("completed scoped-model listing left runtime request active")
	}
}

func TestStaleScopedModelsListingCannotClearNewerRequest(t *testing.T) {
	model := readyModelWithSwitcher(t, &[]string{}).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Pattern: "openai/*"},
		},
	})
	updatedValue, _ := model.handleCommand("/scoped-models")
	updated := testModel(t, updatedValue)
	staleRequest := updated.Model.RuntimeSwitchRequest
	currentRequest := updated.runtimeRequest().begin("newer request")

	next, cmd := updated.handleScopedModelsListed(scopedModelsListedMsg{
		generation: updated.Model.EventGeneration,
		requestID:  staleRequest,
		models: []config.ScopedModel{
			{Provider: "openai", Model: "gpt-4.1-mini"},
		},
	})
	if cmd != nil {
		t.Fatal("stale scoped-model listing returned a command")
	}
	if next.Model.RuntimeSwitchRequest != currentRequest {
		t.Fatalf("active runtime request = %d, want newer request %d", next.Model.RuntimeSwitchRequest, currentRequest)
	}
	next.Close()
}

func TestScopedPatternCatalogResultStartsRuntimeSwitch(t *testing.T) {
	var observed []string
	catalog := modelCatalogStub{
		list: func(context.Context, *config.Config) ([]llm.ModelMetadata, error) {
			return []llm.ModelMetadata{
				{Provider: "openai", ID: "gpt-4.1"},
				{Provider: "openai", ID: "gpt-4.1-mini"},
			}, nil
		},
	}
	model := readyModelWithSwitcher(t, &observed).WithModelCatalog(catalog).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Pattern: "openai/*"},
		},
	})

	updatedValue, cmd := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	if cmd == nil {
		t.Fatal("Ctrl+L returned no scoped-model command")
	}
	message := cmd()
	loaded, ok := message.(scopedModelsLoadedMsg)
	if !ok {
		t.Fatalf("scoped-model result = %T, want scopedModelsLoadedMsg", message)
	}
	updated := testModel(t, updatedValue)
	next, switchCmd := updated.Update(loaded)
	updated = testModel(t, next)
	if switchCmd == nil {
		t.Fatal("scoped-model completion returned no runtime switch")
	}
	switchMessage := switchCmd()
	switched, ok := switchMessage.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("runtime switch result = %T, want runtimeSwitchedMsg", switchMessage)
	}
	updatedValue, _ = updated.Update(switched)
	updated = testModel(t, updatedValue)
	if got, want := updated.Model.Info.Model(), "gpt-4.1-mini"; got != want {
		t.Fatalf("model = %q, want %q", got, want)
	}
	if len(observed) != 1 || observed[0] != "gpt-4.1-mini" {
		t.Fatalf("switched models = %#v, want [gpt-4.1-mini]", observed)
	}
}

func TestStaleScopedPatternResultCannotStartRuntimeSwitch(t *testing.T) {
	var observed []string
	model := readyModelWithSwitcher(t, &observed).WithConfig(&config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
		ScopedModels: []config.ScopedModel{
			{Pattern: "openai/*"},
		},
	})
	updatedValue, _ := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	updated := testModel(t, updatedValue)
	staleRequest := updated.Model.RuntimeSwitchRequest
	currentRequest := updated.runtimeRequest().begin("newer request")

	next, cmd := updated.handleScopedModelsLoaded(scopedModelsLoadedMsg{
		generation: updated.Model.EventGeneration,
		requestID:  staleRequest,
		cfg:        config.Config{Provider: "openai", Model: "gpt-4.1"},
		runtimeCfg: config.Config{Provider: "openai", Model: "gpt-4.1"},
		models: []config.ScopedModel{
			{Provider: "openai", Model: "gpt-4.1-mini"},
		},
		preset:  PresetPrimary,
		forward: true,
	})
	if cmd != nil {
		t.Fatal("stale scoped-model result started a command")
	}
	if next.Model.RuntimeSwitchRequest != currentRequest {
		t.Fatalf("active runtime request = %d, want newer request %d", next.Model.RuntimeSwitchRequest, currentRequest)
	}
	if len(observed) != 0 {
		t.Fatalf("stale scoped-model result switched models = %#v", observed)
	}
	next.Close()
}

func TestCurrentResumeLeafIDDoesNotUseStableSessionIdentity(t *testing.T) {
	model := readyModel(t)
	model.Model.Runtime = Snapshot{
		SessionID:    "stable-session-id",
		Materialized: true,
	}
	model.Model.LeafID = "conversation-leaf-id"

	if got := model.currentResumeLeafID(); got != "conversation-leaf-id" {
		t.Fatalf("resume leaf = %q, want conversation-leaf-id", got)
	}

	model.Model.LeafID = ""
	if got := model.currentResumeLeafID(); got != "" {
		t.Fatalf("resume leaf without a selected entry = %q, want empty", got)
	}
}

func TestDirectModelCommandRequiresIdle(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true

	_, cmd := model.handleCommand("/model model-b")
	if cmd == nil {
		t.Fatal("model command while a turn is active returned no guard")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "Finish or cancel the current turn") {
		t.Fatalf("error = %v, want busy-turn guard", err)
	}
}

type lookupSessionStore struct {
	resumeOnlyStore
	info session.SessionInfoEntry
}

func (s *lookupSessionStore) GetSessionInfo(context.Context, string) (session.SessionInfoEntry, error) {
	return s.info, nil
}

func TestStoredSessionConfigUsesDirectCatalogLookupForForeignWorkdir(t *testing.T) {
	model := readyModel(t)
	store := &lookupSessionStore{
		info: session.SessionInfoEntry{
			EntryBase: session.EntryBase{ID: "foreign-session"},
			Model:     "openai/gpt-4.1",
		},
	}

	cfg, err := model.storedSessionConfig(context.Background(), store, "foreign-session")
	if err != nil {
		t.Fatalf("storedSessionConfig() error = %v", err)
	}
	if cfg.Provider != "openai" || cfg.Model != "gpt-4.1" {
		t.Fatalf("stored session config = %s/%s, want openai/gpt-4.1", cfg.Provider, cfg.Model)
	}
}

type resumeProjectionRunner struct {
	*stubRunner
	projection    agent.SessionProjection
	projectionErr error
	closed        int
}

func (r *resumeProjectionRunner) SessionProjection(context.Context) (agent.SessionProjection, error) {
	return r.projection, r.projectionErr
}

func (r *resumeProjectionRunner) Close() error {
	r.closed++
	return nil
}

type resumeProjectionStorage struct {
	entries      []session.Entry
	entriesCalls int
	metaCalls    int
	branch       string
}

func (s *resumeProjectionStorage) Entries(context.Context) ([]session.Entry, error) {
	s.entriesCalls++
	return append([]session.Entry(nil), s.entries...), nil
}

func (s *resumeProjectionStorage) ID() string { return "resumed" }

func (s *resumeProjectionStorage) Meta() session.Metadata {
	s.metaCalls++
	return session.Metadata{ID: s.ID(), Branch: s.branch}
}

func (s *resumeProjectionStorage) Usage(context.Context) (session.Usage, error) {
	return session.Usage{}, nil
}

func TestResumeReplaysRuntimeOwnedBranchProjection(t *testing.T) {
	active := []session.Entry{sysEntry("active branch")}
	abandoned := []session.Entry{sysEntry("abandoned branch")}
	runner := &resumeProjectionRunner{
		stubRunner: &stubRunner{},
		projection: agent.SessionProjection{
			ID:             "resumed",
			LeafID:         active[0].ID(),
			Branch:         active,
			WorktreeBranch: "feature/resumed",
		},
	}
	storage := &resumeProjectionStorage{entries: abandoned, branch: "stale-storage-branch"}
	model := readyModel(t)
	model.Model.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		return stubBackend{}, runner, storage, nil
	}

	updated, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "test-model"},
		sysEntry("resume"),
		"resumed",
	)
	if cmd == nil {
		t.Fatal("resume command is nil")
	}
	msg, ok := cmd().(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("resume result = %T, want runtimeSwitchedMsg", cmd())
	}
	if storage.entriesCalls != 0 {
		t.Fatalf("storage Entries calls = %d, want 0", storage.entriesCalls)
	}
	if len(msg.replayEntries) != len(active) || session.EntryText(msg.replayEntries[0]) != "active branch" {
		t.Fatalf("replay entries = %#v, want active branch only", msg.replayEntries)
	}
	if msg.leafID != active[0].ID() {
		t.Fatalf("replay leaf = %q, want %q", msg.leafID, active[0].ID())
	}
	if !strings.Contains(strings.Join(msg.printLines, "\n"), "feature/resumed") {
		t.Fatalf("resume header = %q, want runtime-owned worktree branch", strings.Join(msg.printLines, "\n"))
	}
	if runner.closed != 0 {
		t.Fatalf("replacement runner close count = %d, want 0 before installation", runner.closed)
	}
	_ = updated
}

func TestResumeProjectionFailureClosesReplacement(t *testing.T) {
	projectionErr := errors.New("projection unavailable")
	previousSave := saveRuntimeState
	saved := false
	saveRuntimeState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}
	defer func() { saveRuntimeState = previousSave }()
	runner := &resumeProjectionRunner{
		stubRunner:    &stubRunner{},
		projectionErr: projectionErr,
	}
	storage := &resumeProjectionStorage{}
	model := readyModel(t)
	model.Model.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		return stubBackend{}, runner, storage, nil
	}

	_, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "test-model"},
		sysEntry("resume"),
		"resumed",
	)
	if cmd == nil {
		t.Fatal("resume command is nil")
	}
	msg, ok := cmd().(runtimeSwitchErrorMsg)
	if !ok {
		t.Fatalf("resume result = %T, want runtimeSwitchErrorMsg", cmd())
	}
	if !errors.Is(msg.err, projectionErr) {
		t.Fatalf("resume error = %v, want projection error", msg.err)
	}
	if runner.closed != 1 {
		t.Fatalf("replacement runner close count = %d, want 1", runner.closed)
	}
	if saved {
		t.Fatal("projection-rejected replacement persisted runtime state")
	}
	if storage.entriesCalls != 0 {
		t.Fatalf("storage Entries calls = %d, want 0", storage.entriesCalls)
	}
}

func TestRuntimeSwitchUsesAcceptedWorktreeBranch(t *testing.T) {
	model := readyModel(t)
	model.Model.RuntimeSwitchRequest = 1
	oldRunner := &stubRunner{}
	newRunner := &stubRunner{}
	storage := &resumeProjectionStorage{branch: "stale-storage-branch"}

	model.Model.Runner = oldRunner
	model, _ = model.handleRuntimeSwitched(runtimeSwitchedMsg{
		switchID:       1,
		runtime:        Accepted{Handles: Handles{Runner: newRunner, Storage: storage}},
		previous:       Handles{Runner: oldRunner},
		worktreeBranch: "accepted-runtime-branch",
	})
	if model.App.Branch != "accepted-runtime-branch" {
		t.Fatalf("installed worktree branch = %q, want accepted-runtime-branch", model.App.Branch)
	}
	if storage.metaCalls != 0 {
		t.Fatalf("storage Meta calls = %d, want 0 after runtime acceptance", storage.metaCalls)
	}
}

func TestRuntimeSwitchUsesCancelableRequestContext(t *testing.T) {
	started := make(chan struct{})
	seen := make(chan context.Context, 1)
	switcher := func(ctx context.Context, _ *config.Config, _ string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		seen <- ctx
		close(started)
		<-ctx.Done()
		return nil, nil, nil, ctx.Err()
	}
	model := readyModel(t)
	model.Model.Switcher = switcher

	updated, cmd := model.switchRuntimeCommand(
		Transition{Snapshot: Snapshot{}},
		sysEntry("switch"),
		"",
		false,
	)
	if cmd == nil {
		t.Fatal("runtime switch returned no command")
	}
	requestContext := updated.runtimeRequestOperationContext()
	resultCh := make(chan any, 1)
	go func() { resultCh <- cmd() }()
	<-started
	if got := <-seen; got != requestContext {
		t.Fatalf("switch context = %v, want request context", got)
	}

	updated.rotateRuntimeContext()
	result, ok := (<-resultCh).(runtimeSwitchErrorMsg)
	if !ok || !errors.Is(result.err, context.Canceled) {
		t.Fatalf("canceled switch result = %#v, want context cancellation", result)
	}
}
