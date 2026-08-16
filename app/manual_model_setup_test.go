package app

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/agent"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

type modelCatalogStub struct {
	list  func(context.Context, *config.Config) ([]llm.ModelMetadata, error)
	query func(context.Context, llm.ModelCatalogQuery) (llm.ModelCatalogResult, error)
}

func (s modelCatalogStub) ListModelsForConfig(ctx context.Context, cfg *config.Config) ([]llm.ModelMetadata, error) {
	if s.list == nil {
		return nil, nil
	}
	return s.list(ctx, cfg)
}

func (s modelCatalogStub) QueryAvailableModels(
	ctx context.Context,
	query llm.ModelCatalogQuery,
) (llm.ModelCatalogResult, error) {
	if s.query == nil {
		return llm.ModelCatalogResult{}, nil
	}
	return s.query(ctx, query)
}

func TestProviderSetupUsesOAuthForCodex(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	setup, err := providerSetupPrompt(t.Context(), &config.Config{Provider: "openai-codex"}, nil)
	if err != nil {
		t.Fatalf("provider setup: %v", err)
	}
	if setup != SetupPromptOAuth {
		t.Fatalf("setup = %d, want OAuth setup", setup)
	}
}

func TestModelSelectionForNonListingProviderOpensManualPrompt(t *testing.T) {
	model := readyModel(t).WithConfig(&config.Config{Provider: "moonshot"})

	updated, cmd := model.openModelSelectionForPreset(model.Model.Config, PresetPrimary)
	if cmd != nil {
		t.Fatal("manual model setup returned an unexpected command")
	}
	if updated.Picker.Setup == nil {
		t.Fatal("manual model setup did not open a prompt")
	}
	if got, want := updated.Picker.Setup.kind, SetupPromptModelID; got != want {
		t.Fatalf("setup kind = %d, want %d", got, want)
	}
	if got := updated.Picker.Setup.provider; got != "moonshot" {
		t.Fatalf("provider = %q, want moonshot", got)
	}
}

func TestManualModelPickerItemSurvivesCatalogLoad(t *testing.T) {
	model := readyModel(t)
	cfg := &config.Config{Provider: "moonshot"}

	items := model.modelPickerItemsForCatalog(cfg, nil, nil)
	if len(items) != 1 || !items[0].ManualModel {
		t.Fatalf("picker items = %#v, want one manual model item", items)
	}
}

func TestModelItemsFromMetadataPreservesProviderGroups(t *testing.T) {
	items := modelItemsFromMetadata([]llm.ModelMetadata{
		{ID: "shared-model", Provider: "openrouter"},
		{ID: "shared-model", Provider: "openai"},
	})
	if len(items) != 2 {
		t.Fatalf("items = %#v, want both provider entries", items)
	}
	groups := make(map[string]string, len(items))
	for _, item := range items {
		groups[item.Provider] = item.Group
	}
	if groups["openrouter"] != "OpenRouter" || groups["openai"] != "OpenAI" {
		t.Fatalf("provider groups = %#v, want openrouter/OpenRouter and openai/OpenAI", groups)
	}
	if items[0].Provider != "openai" || items[1].Provider != "openrouter" {
		t.Fatalf("items = %#v, want provider-contiguous order", items)
	}
}

func TestModelPickerKeepsModelsBoundToSelectedProvider(t *testing.T) {
	model := readyModel(t)
	cfg := &config.Config{
		Provider: "openai",
		Model:    "shared-model",
	}
	all := modelItemsFromMetadata([]llm.ModelMetadata{
		{ID: "shared-model", Provider: "openai"},
		{ID: "shared-model", Provider: "openrouter"},
	})
	favorites := model.modelPickerFavoriteItems(cfg, all)
	if len(favorites) != 1 || favorites[0].Provider != "openai" {
		t.Fatalf("favorites = %#v, want current openai model", favorites)
	}
	combined := model.modelPickerItemsForCatalog(cfg, favorites, all)
	for _, item := range combined {
		if item.Value != "" && item.Provider != "openai" {
			t.Fatalf("model picker item = %#v, want only openai models", item)
		}
	}
	if len(combined) != 2 {
		t.Fatalf("picker items = %#v, want current model and one manual entry", combined)
	}
}

func TestModelPickerQueriesOnlySelectedProvider(t *testing.T) {
	var observed llm.ModelCatalogQuery
	catalog := modelCatalogStub{
		query: func(_ context.Context, query llm.ModelCatalogQuery) (llm.ModelCatalogResult, error) {
			observed = query
			return llm.ModelCatalogResult{
				Models: []llm.ModelMetadata{
					{ID: "openai-model", Provider: "openai"},
					{ID: "router-model", Provider: "openrouter"},
					{ID: "local-model", Provider: "ollama"},
				},
			}, nil
		},
	}
	model := readyModel(t).
		WithModelCatalog(catalog).
		WithConfig(&config.Config{Provider: "openai", Model: "openai-model"})
	opened, cmd := model.openModelPickerForPreset(model.Model.Config, PresetPrimary)
	message := cmd()
	loaded, ok := message.(modelPickerLoadedMsg)
	if !ok {
		t.Fatalf("load result = %T, want modelPickerLoadedMsg", message)
	}
	if len(observed.Providers) != 1 || observed.Providers[0] != "openai" {
		t.Fatalf("providers = %#v, want [openai]", observed.Providers)
	}
	if observed.IncludeLocal {
		t.Fatal("provider-scoped model picker enabled default local discovery")
	}
	if len(loaded.items) != 1 || loaded.items[0].Provider != "openai" {
		t.Fatalf("loaded items = %#v, want only the selected provider", loaded.items)
	}
	updated, _ := opened.handleModelPickerLoaded(loaded)
	for _, item := range updated.Picker.Overlay.items {
		if item.Value != "" && item.Provider != "openai" {
			t.Fatalf("picker item = %#v, want only openai models", item)
		}
	}
}

func TestModelPickerLoadHonorsOverlayCancellation(t *testing.T) {
	modelCatalog := modelCatalogStub{
		query: func(ctx context.Context, query llm.ModelCatalogQuery) (llm.ModelCatalogResult, error) {
			<-ctx.Done()
			return llm.ModelCatalogResult{}, ctx.Err()
		},
	}

	ctx, cancel := context.WithCancel(t.Context())
	model := readyModel(t).WithModelCatalog(modelCatalog)
	model.Picker.Overlay = &pickerOverlayState{
		purpose:     pickerPurposeModel,
		request:     1,
		cfg:         &config.Config{Provider: "openai"},
		loading:     true,
		loadContext: ctx,
		loadCancel:  cancel,
	}
	loadCfg := *model.Picker.Overlay.cfg
	done := make(chan any, 1)
	go func() {
		done <- loadModelPickerItems(1, &loadCfg, PresetPrimary, ctx, model.Model.Catalog)()
	}()

	model.pickerReducer().closeOverlay()
	select {
	case msg := <-done:
		loaded, ok := msg.(modelPickerLoadedMsg)
		if !ok || !errors.Is(loaded.err, context.Canceled) {
			t.Fatalf("load result = %#v, want canceled model load", msg)
		}
	case <-time.After(time.Second):
		t.Fatal("canceled model load did not settle")
	}
}

func TestModelCatalogWarningSurfacesPartialAndStaleProviders(t *testing.T) {
	warning := modelCatalogWarning(llm.ModelCatalogResult{
		Status: []llm.ModelCatalogStatus{
			{Provider: "openai", Err: errors.New("offline")},
			{Provider: "openrouter", Stale: true},
		},
	})
	if !strings.Contains(warning, "OpenAI") || !strings.Contains(warning, "OpenRouter") {
		t.Fatalf("warning = %q, want unavailable and stale providers", warning)
	}
}

func TestManualModelPickerActionOpensEmptyPrompt(t *testing.T) {
	model := readyModel(t).WithConfig(&config.Config{Provider: "moonshot"})
	model.Picker.Overlay = &pickerOverlayState{
		purpose: pickerPurposeModel,
		preset:  PresetPrimary,
		cfg:     &config.Config{Provider: "moonshot"},
		items:   []pickerItem{manualModelPickerItem()},
		filtered: []pickerItem{
			manualModelPickerItem(),
		},
	}

	updated, cmd := model.commitPickerSelection()
	if cmd != nil {
		t.Fatal("manual picker action returned an unexpected command")
	}
	if updated.Picker.Setup == nil || updated.Picker.Setup.kind != SetupPromptModelID {
		t.Fatalf("setup = %#v, want model-ID prompt", updated.Picker.Setup)
	}
	if updated.Picker.Setup.value != "" {
		t.Fatalf("model-ID prompt value = %q, want empty input", updated.Picker.Setup.value)
	}
}

func TestProviderSetupSaveLeadsToManualModelPrompt(t *testing.T) {
	model := readyModel(t)
	model.Picker.Setup = &setupPromptState{kind: SetupPromptAPIKey}
	requestID, ok := model.pickerReducer().beginSetupSave()
	if !ok {
		t.Fatal("beginSetupSave returned false")
	}

	updated, cmd := model.handleSetupPromptSaved(setupPromptSavedMsg{
		requestID: requestID,
		cfg: config.Config{
			Provider: "moonshot",
		},
		preset: PresetPrimary,
	})
	if cmd != nil {
		t.Fatal("provider setup completion returned an unexpected command")
	}
	if updated.Picker.Setup == nil || updated.Picker.Setup.kind != SetupPromptModelID {
		t.Fatalf("setup = %#v, want manual model-ID prompt", updated.Picker.Setup)
	}
}

func TestProviderSetupPickerDefersCredentialReads(t *testing.T) {
	model := readyModel(t)
	updated, cmd := model.openProviderSetupPicker()
	if cmd == nil {
		t.Fatal("provider picker returned no deferred command")
	}
	if updated.Picker.Overlay == nil || !updated.Picker.Overlay.loading {
		t.Fatal("provider picker did not preserve a loading overlay before metadata")
	}
	if updated.Model.RuntimeSwitchRequest == 0 {
		t.Fatal("provider picker did not own a runtime request")
	}
	message := cmd()
	loaded, ok := message.(providerItemsLoadedMsg)
	if !ok {
		t.Fatalf("provider picker result = %T, want providerItemsLoadedMsg", message)
	}
	next, completionCmd := updated.Update(loaded)
	if completionCmd != nil {
		t.Fatal("provider picker completion returned an unexpected command")
	}
	final := testModel(t, next)
	if final.Model.RuntimeSwitchRequest != 0 {
		t.Fatal("provider picker completion left runtime request active")
	}
	if final.Picker.Overlay == nil || final.Picker.Overlay.purpose != pickerPurposeProviderSetup {
		t.Fatalf("overlay = %#v, want provider setup picker", final.Picker.Overlay)
	}
}

func TestCancelProviderPickerStopsPendingLoad(t *testing.T) {
	model := readyModel(t)
	updated, cmd := model.openProviderSetupPicker()
	if cmd == nil {
		t.Fatal("provider picker returned no deferred command")
	}
	updated, closeCmd := updated.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if closeCmd != nil {
		t.Fatal("provider picker cancel returned an unexpected command")
	}
	if updated.Model.RuntimeSwitchRequest != 0 {
		t.Fatal("provider picker cancel left runtime request active")
	}
	if updated.Picker.Overlay != nil {
		t.Fatal("provider picker cancel left overlay open")
	}
	message := cmd()
	next, completionCmd := updated.Update(message)
	if completionCmd != nil {
		t.Fatal("canceled provider picker completion returned a command")
	}
	if testModel(t, next).Picker.Overlay != nil {
		t.Fatal("canceled provider picker completion reopened the overlay")
	}
}

func TestWithProviderPickerPreservesStartupLoad(t *testing.T) {
	model := readyModel(t).WithProviderPicker()
	if model.Picker.Overlay == nil || !model.Picker.Overlay.loading {
		t.Fatal("WithProviderPicker did not preserve a loading overlay")
	}
	cmd := model.startupPickerCmd()
	if cmd == nil {
		t.Fatal("startup provider picker returned no load command")
	}
	message := cmd()
	loaded, ok := message.(providerItemsLoadedMsg)
	if !ok {
		t.Fatalf("startup provider picker result = %T, want providerItemsLoadedMsg", message)
	}
	next, completionCmd := model.Update(loaded)
	if completionCmd != nil {
		t.Fatal("startup provider picker completion returned an unexpected command")
	}
	final := testModel(t, next)
	if final.Picker.Overlay == nil || final.Picker.Overlay.purpose != pickerPurposeProviderSetup {
		t.Fatalf("overlay = %#v, want provider setup picker", final.Picker.Overlay)
	}
}

func TestStaleProviderItemsCompletionCannotOpenReplacementPicker(t *testing.T) {
	model := readyModel(t)
	updated, cmd := model.openProviderSetupPicker()
	if cmd == nil {
		t.Fatal("provider picker returned no deferred command")
	}
	requestID := updated.Model.RuntimeSwitchRequest
	generation := updated.Model.EventGeneration
	updated.rotateRuntimeContext()
	updated.runtimeRequest().clear()
	updated.Model.EventGeneration++
	updated.pickerReducer().closeOverlay()

	next, completionCmd := updated.Update(providerItemsLoadedMsg{
		generation: generation,
		requestID:  requestID,
		items:      []pickerItem{{Label: "Ollama", Value: "ollama"}},
	})
	if completionCmd != nil {
		t.Fatal("stale provider picker completion returned a command")
	}
	final := testModel(t, next)
	if final.Picker.Overlay != nil {
		t.Fatal("stale provider picker completion opened replacement UI")
	}
}

func TestProviderSetupDoesNotProbeCatalogSynchronously(t *testing.T) {
	modelCatalog := modelCatalogStub{list: func(context.Context, *config.Config) ([]llm.ModelMetadata, error) {
		t.Fatal("provider setup synchronously probed model catalog")
		return nil, nil
	}}
	t.Setenv("OPENAI_API_KEY", "test-key")

	model := readyModel(t).WithModelCatalog(modelCatalog).WithConfig(&config.Config{Provider: "openai"})
	updated, cmd := model.handleProviderCommand("openai")
	if cmd == nil {
		t.Fatal("provider setup did not start asynchronous setup resolution")
	}
	setupMsg, ok := cmd().(providerSetupResolvedMsg)
	if !ok {
		t.Fatalf("provider setup result = %T, want providerSetupResolvedMsg", cmd())
	}
	next, _ := updated.Update(setupMsg)
	updated = testModel(t, next)
	if updated.Picker.Overlay == nil || updated.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("overlay = %#v, want model picker", updated.Picker.Overlay)
	}
}

func TestOpenAICompatibleProviderProbeRunsOutsideCommandHandlingAndCancels(t *testing.T) {
	probeStarted := make(chan struct{})
	probeCanceled := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		close(probeStarted)
		<-r.Context().Done()
		close(probeCanceled)
	}))
	defer server.Close()

	resolver := llm.NewEndpointResolver(llm.EndpointResolverOptions{HTTPClient: server.Client()})
	model := readyModel(t).WithEndpointResolver(resolver).WithConfig(&config.Config{
		Provider: "openai-compatible",
		Endpoint: server.URL + "/v1",
	})

	resultCh := make(chan struct {
		model Model
		cmd   tea.Cmd
	}, 1)
	go func() {
		updated, cmd := model.handleCommand("/provider openai-compatible")
		resultCh <- struct {
			model Model
			cmd   tea.Cmd
		}{updated, cmd}
	}()

	var result struct {
		model Model
		cmd   tea.Cmd
	}
	select {
	case result = <-resultCh:
	case <-time.After(time.Second):
		t.Fatal("provider command blocked while probing OpenAI-compatible endpoint")
	}
	select {
	case <-probeStarted:
		t.Fatal("endpoint probe started inside provider command handling")
	default:
	}
	if result.cmd == nil {
		t.Fatal("provider command returned no asynchronous setup command")
	}
	if result.model.Model.RuntimeSwitchRequest == 0 {
		t.Fatal("provider setup did not own a runtime request")
	}

	messageCh := make(chan tea.Msg, 1)
	go func() { messageCh <- result.cmd() }()
	select {
	case <-probeStarted:
	case <-time.After(time.Second):
		t.Fatal("provider setup command did not start endpoint probe")
	}
	result.model.Close()
	select {
	case <-probeCanceled:
	case <-time.After(time.Second):
		t.Fatal("endpoint probe did not observe provider setup cancellation")
	}

	var message tea.Msg
	select {
	case message = <-messageCh:
	case <-time.After(time.Second):
		t.Fatal("provider setup command did not settle after cancellation")
	}
	resolved, ok := message.(providerSetupResolvedMsg)
	if !ok || !errors.Is(resolved.err, context.Canceled) {
		t.Fatalf("provider setup result = %#v, want canceled providerSetupResolvedMsg", message)
	}
	settled, _ := result.model.Update(resolved)
	if next := testModel(t, settled); next.Model.RuntimeSwitchRequest != 0 {
		t.Fatal("canceled provider setup left runtime request active")
	}
}

func TestProviderSetupCompletionOpensModelPicker(t *testing.T) {
	model := readyModel(t).WithConfig(&config.Config{
		Provider: "openai-compatible",
		Endpoint: "http://127.0.0.1:8080/v1",
	})
	requestID := model.runtimeRequest().begin("Checking provider endpoint...")
	generation := model.Model.EventGeneration

	updated, cmd := model.Update(providerSetupResolvedMsg{
		generation: generation,
		requestID:  requestID,
		cfg: config.Config{
			Provider: "openai-compatible",
			Endpoint: "http://127.0.0.1:8080/v1",
		},
		preset: PresetPrimary,
	})
	if cmd == nil {
		t.Fatal("provider setup completion returned no asynchronous model load")
	}
	final := testModel(t, updated)
	if final.Model.RuntimeSwitchRequest != 0 {
		t.Fatal("provider setup completion left runtime request active")
	}
	if final.Picker.Overlay == nil || final.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("overlay = %#v, want model picker", final.Picker.Overlay)
	}
}

func TestStaleProviderSetupCompletionCannotOpenPicker(t *testing.T) {
	model := readyModel(t).WithConfig(&config.Config{Provider: "openai-compatible"})
	requestID := model.runtimeRequest().begin("Checking provider endpoint...")
	generation := model.Model.EventGeneration
	model.rotateRuntimeContext()
	model.runtimeRequest().clear()
	model.Model.EventGeneration++

	updated, cmd := model.Update(providerSetupResolvedMsg{
		generation: generation,
		requestID:  requestID,
		cfg:        config.Config{Provider: "openai-compatible"},
		preset:     PresetPrimary,
	})
	if cmd != nil {
		t.Fatal("stale provider setup completion returned a command")
	}
	final := testModel(t, updated)
	if final.Picker.Setup != nil || final.Picker.Overlay != nil {
		t.Fatal("stale provider setup completion opened UI in the replacement runtime")
	}
}

func TestProviderPickerUsesNativeSetupForOllama(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		purpose: pickerPurposeProviderSetup,
		cfg:     &config.Config{},
		items:   []pickerItem{{Label: "Ollama", Value: "ollama"}},
		filtered: []pickerItem{{
			Label: "Ollama",
			Value: "ollama",
		}},
	}

	updated, cmd := model.commitPickerSelection()
	if cmd == nil {
		t.Fatal("Ollama provider selection did not start asynchronous setup resolution")
	}
	setupMsg, ok := cmd().(providerSetupResolvedMsg)
	if !ok {
		t.Fatalf("provider setup result = %T, want providerSetupResolvedMsg", cmd())
	}
	next, _ := updated.Update(setupMsg)
	updated = testModel(t, next)
	if updated.Picker.Setup != nil {
		t.Fatalf("setup = %#v, want no API-key prompt", updated.Picker.Setup)
	}
	if updated.Picker.Overlay == nil || updated.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("overlay = %#v, want model picker", updated.Picker.Overlay)
	}
}

func TestManualModelPromptCommitsArbitraryModelID(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	var observed []string
	model := readyModelWithSwitcher(t, &observed).WithConfig(&config.Config{
		Provider: "moonshot",
	})
	model.Picker.Setup = &setupPromptState{
		kind:   SetupPromptModelID,
		value:  "moonshot-v1-8k",
		preset: PresetPrimary,
		cfg: config.Config{
			Provider: "moonshot",
		},
	}

	updated, cmd := model.commitSetupPrompt()
	if cmd == nil {
		t.Fatal("manual model prompt did not start a runtime switch")
	}
	if updated.Picker.Setup != nil {
		t.Fatal("manual model prompt remained open after submit")
	}

	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("command result = %T, want runtimeSwitchedMsg", msg)
	}
	final, _ := updated.Update(switched)
	updated = testModel(t, final)
	if got, want := updated.Model.Info.Model(), "moonshot-v1-8k"; got != want {
		t.Fatalf("backend model = %q, want %q", got, want)
	}
	if len(observed) != 1 || observed[0] != "moonshot-v1-8k" {
		t.Fatalf("switched models = %#v, want arbitrary model ID", observed)
	}
}

func TestManualModelPromptRestoresRetryAfterSwitchFailure(t *testing.T) {
	model := readyModel(t).WithConfig(&config.Config{Provider: "moonshot"})
	model.Model.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		return nil, nil, nil, errors.New("provider unavailable")
	}
	model.Picker.Setup = &setupPromptState{
		kind:   SetupPromptModelID,
		value:  "org/model:latest",
		preset: PresetPrimary,
		cfg:    config.Config{Provider: "moonshot"},
	}

	updated, cmd := model.commitSetupPrompt()
	if cmd == nil {
		t.Fatal("manual model prompt did not start the failing runtime switch")
	}
	msg, ok := cmd().(runtimeSwitchErrorMsg)
	if !ok {
		t.Fatalf("command result = %T, want runtimeSwitchErrorMsg", msg)
	}
	final, _ := updated.Update(msg)
	updated = testModel(t, final)
	if updated.Picker.Setup == nil {
		t.Fatal("failed manual model switch did not restore the prompt")
	}
	if got := updated.Picker.Setup.value; got != "org/model:latest" {
		t.Fatalf("restored model ID = %q, want original input", got)
	}
	if got := updated.Picker.Setup.err; !strings.Contains(got, "provider unavailable") {
		t.Fatalf("restored error = %q, want provider failure", got)
	}
}

func TestManualModelPromptRestoresRetryAfterPersistenceFailure(t *testing.T) {
	model := readyModel(t).WithConfig(&config.Config{Provider: "moonshot"})
	previousSave := saveRuntimeState
	saveRuntimeState = func(config.RuntimeStateUpdate) error {
		return errors.New("state unavailable")
	}
	defer func() { saveRuntimeState = previousSave }()
	model.Picker.Setup = &setupPromptState{
		kind:   SetupPromptModelID,
		value:  "moonshot-v1-8k",
		preset: PresetPrimary,
		cfg:    config.Config{Provider: "moonshot"},
	}

	updated, cmd := model.commitSetupPrompt()
	if cmd == nil {
		t.Fatal("manual model prompt did not start persistence")
	}
	msg, ok := cmd().(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("command result = %T, want TransitionCommittedMsg", msg)
	}
	final, _ := updated.Update(msg)
	updated = testModel(t, final)
	if updated.Picker.Setup == nil || updated.Picker.Setup.value != "moonshot-v1-8k" {
		t.Fatalf("setup = %#v, want retryable model prompt", updated.Picker.Setup)
	}
	if !strings.Contains(updated.Picker.Setup.err, "state unavailable") {
		t.Fatalf("setup error = %q, want persistence failure", updated.Picker.Setup.err)
	}
}

func TestManualModelPromptRejectsEmptyID(t *testing.T) {
	model := readyModel(t)
	model.Picker.Setup = &setupPromptState{
		kind: SetupPromptModelID,
		cfg: config.Config{
			Provider: "moonshot",
		},
	}

	updated, cmd := model.commitSetupPrompt()
	if cmd != nil {
		t.Fatal("empty model ID returned a command")
	}
	if updated.Picker.Setup == nil || updated.Picker.Setup.err != "model ID cannot be empty" {
		t.Fatalf("setup state = %#v, want visible empty-ID error", updated.Picker.Setup)
	}
}
