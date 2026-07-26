package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
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

func TestModelPickerKeepsDuplicateIDsBoundToTheirProvider(t *testing.T) {
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
	providers := make(map[string]bool)
	for _, item := range combined {
		if item.Value == "shared-model" {
			providers[item.Provider] = true
		}
	}
	if len(providers) != 2 || !providers["openai"] || !providers["openrouter"] {
		t.Fatalf("duplicate model providers = %#v, want both providers", providers)
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
		done <- loadAllModelPickerItems(1, &loadCfg, PresetPrimary, ctx, model.Model.Catalog)()
	}()

	model.pickerReducer().closeOverlay()
	select {
	case msg := <-done:
		loaded, ok := msg.(allModelsLoadedMsg)
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

func TestProviderSetupDoesNotProbeCatalogSynchronously(t *testing.T) {
	modelCatalog := modelCatalogStub{list: func(context.Context, *config.Config) ([]llm.ModelMetadata, error) {
		t.Fatal("provider setup synchronously probed model catalog")
		return nil, nil
	}}
	t.Setenv("OPENAI_API_KEY", "test-key")

	model := readyModel(t).WithModelCatalog(modelCatalog).WithConfig(&config.Config{Provider: "openai"})
	updated, cmd := model.handleProviderCommand("openai")
	if cmd == nil {
		t.Fatal("provider setup did not start asynchronous model loading")
	}
	if updated.Picker.Overlay == nil || updated.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("overlay = %#v, want model picker", updated.Picker.Overlay)
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
		t.Fatal("Ollama provider selection did not start model loading")
	}
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
