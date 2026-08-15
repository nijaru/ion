package app

import (
	"context"
	"errors"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func pickerSelectionRequiresIdle(purpose pickerPurpose) bool {
	switch purpose {
	case pickerPurposeModel, pickerPurposeThinking:
		return true
	default:
		return false
	}
}

func ensureProviderReadyForSelection(ctx context.Context, cfg *config.Config, resolver *llm.EndpointResolver) error {
	if cfg == nil || !llm.IsOpenAICompatible(cfg.Provider) {
		return nil
	}
	if resolver == nil {
		return errors.New("OpenAI-compatible endpoint resolver is unavailable")
	}
	if _, ready := resolver.ProbeFresh(ctx, cfg); ready {
		return nil
	}
	if strings.TrimSpace(cfg.Endpoint) != "" {
		return errors.New("OpenAI-compatible endpoint is not running")
	}
	return errors.New("set an endpoint or start a local OpenAI-compatible server")
}

// openModelPicker opens the unified model picker showing models from all
// configured providers grouped by provider name.
func (m Model) openModelPicker() (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openModelPickerWithConfig(cfg)
}

func (m Model) openModelPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	return m.openModelPickerForPreset(cfg, m.activePreset())
}

func (m Model) openModelPickerForPreset(
	cfg *config.Config,
	preset Preset,
) (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing runtime settings"))
	}
	if cfg == nil {
		cfg = &config.Config{}
	}
	loadContext, loadCancel := context.WithCancel(context.Background())

	// Build initial items from favorites + any cached models
	favorites := m.modelPickerFavoriteItems(cfg, nil)
	if strings.TrimSpace(cfg.Provider) != "" {
		favorites = append(favorites, manualModelPickerItem())
	}
	items := clonePickerItems(favorites)

	m.clearProgressError()
	requestID := m.pickerReducer().beginModelOverlayLoad(pickerOverlayState{
		title:       "Pick a model",
		items:       items,
		filtered:    append([]pickerItem(nil), items...),
		index:       pickerIndexForModel(items, cfg.Provider, configuredModelForPreset(cfg, preset)),
		purpose:     pickerPurposeModel,
		preset:      preset,
		cfg:         cfg,
		loading:     true,
		loadContext: loadContext,
		loadCancel:  loadCancel,
	})
	return m, loadAllModelPickerItems(requestID, cfg, preset, loadContext, m.Model.Catalog)
}

// loadAllModelPickerItems loads the configured provider catalog. Provider
// fan-out and cache/error handling belong to llm; the TUI only projects the
// typed result into picker items.
func loadAllModelPickerItems(
	requestID uint64,
	cfg *config.Config,
	preset Preset,
	loadContext context.Context,
	catalog ModelCatalog,
) tea.Cmd {
	cfgCopy := config.Config{}
	if cfg != nil {
		cfgCopy = *cfg
	}
	if loadContext == nil {
		loadContext = context.Background()
	}
	return func() tea.Msg {
		if catalog == nil {
			return allModelsLoadedMsg{
				requestID: requestID,
				err:       fmt.Errorf("model catalog is not configured"),
			}
		}
		result, err := catalog.QueryAvailableModels(loadContext, llm.ModelCatalogQuery{
			Config:       &cfgCopy,
			IncludeLocal: true,
		})
		return allModelsLoadedMsg{
			requestID: requestID,
			items:     modelItemsFromMetadata(result.Models),
			catalog:   result,
			err:       err,
		}
	}
}

func (m Model) handleAllModelsLoaded(msg allModelsLoadedMsg) (Model, tea.Cmd) {
	if !m.pickerReducer().modelLoadRequestMatches(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		m.pickerReducer().failModelLoad(
			msg.requestID,
			fmt.Sprintf("Failed to load models: %v", msg.err),
		)
		return m, nil
	}

	cfg := m.Picker.Overlay.cfg
	preset := m.Picker.Overlay.Preset()

	// Merge loaded items with favorites
	favorites := m.modelPickerFavoriteItems(cfg, msg.items)
	combined := m.modelPickerItemsForCatalog(cfg, favorites, msg.items)

	if len(combined) == 0 {
		m.pickerReducer().failModelLoad(
			msg.requestID,
			"No models available. Use /login <provider> to set up API keys.",
		)
		return m, nil
	}

	m.pickerReducer().completeModelLoad(
		msg.requestID,
		combined,
		configuredModelForPreset(cfg, preset),
		modelCatalogWarning(msg.catalog),
	)
	return m, nil
}

func modelCatalogWarning(catalog llm.ModelCatalogResult) string {
	var unavailable []string
	var stale []string
	for _, status := range catalog.Status {
		label := providerDisplayName(status.Provider)
		if label == "" {
			label = status.Provider
		}
		if status.Err != nil {
			unavailable = append(unavailable, label)
		}
		if status.Stale {
			stale = append(stale, label)
		}
	}
	if len(unavailable) > 0 && len(stale) > 0 {
		return fmt.Sprintf(
			"Some catalogs unavailable (%s); using cached models for %s",
			strings.Join(unavailable, ", "),
			strings.Join(stale, ", "),
		)
	}
	if len(unavailable) > 0 {
		return "Some catalogs unavailable: " + strings.Join(unavailable, ", ")
	}
	if len(stale) > 0 || catalog.Stale {
		if len(stale) == 0 {
			return "Using cached model metadata"
		}
		return "Using cached model metadata for: " + strings.Join(stale, ", ")
	}
	return ""
}

func (m Model) openThinkingPicker() (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing runtime settings"))
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForActivePreset(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	items := []pickerItem{
		{Label: "Auto", Value: config.DefaultReasoningEffort, Detail: "Provider default"},
		{Label: "Off", Value: "off", Detail: "No reasoning"},
		{Label: "Minimal", Value: "minimal", Detail: "Very brief reasoning (~1k tokens)"},
		{Label: "Low", Value: "low", Detail: "Light reasoning (~2k tokens)"},
		{Label: "Medium", Value: "medium", Detail: "Moderate reasoning (~8k tokens)"},
		{Label: "High", Value: "high", Detail: "Deep reasoning (~16k tokens)"},
		{Label: "XHigh", Value: "xhigh", Detail: "Extra-high reasoning (~32k tokens)"},
		{Label: "Max", Value: "max", Detail: "Maximum reasoning"},
	}
	currentIndex := pickerIndex(items, normalizeThinkingValue(runtimeCfg.ReasoningEffort))
	for i := range items {
		isActive := i == currentIndex
		currentVal := ""
		if isActive {
			currentVal = "active"
		}
		items[i].SettingName = items[i].Label
		items[i].CurrentVal = currentVal
		items[i].Desc = items[i].Detail
		items[i].Search = pickerSearchIndex(
			items[i].Label,
			items[i].Value,
			items[i].Detail,
			"",
			nil,
		)
	}
	m.pickerReducer().openOverlayInvalidatingModelLoads(pickerOverlayState{
		title:    "Pick a " + m.activePresetTitle() + " thinking level",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    currentIndex,
		purpose:  pickerPurposeThinking,
		cfg:      cfg,
	})
	return m, nil
}

func (m Model) modelPickerFavoriteItems(cfg *config.Config, all []pickerItem) []pickerItem {
	if cfg == nil || cfg.Provider == "" {
		return nil
	}

	primaryModel := strings.TrimSpace(cfg.Model)
	fastModel := strings.TrimSpace(cfg.FastModel)
	provider := llm.ResolveID(cfg.Provider)
	switch {
	case primaryModel == "" && fastModel == "":
		return nil
	case primaryModel != "" && strings.EqualFold(primaryModel, fastModel):
		item := m.modelPickerFavoriteItem(all, provider, primaryModel, "primary")
		item.Group = "Current"
		return []pickerItem{item}
	}

	favorites := make([]pickerItem, 0, 2)
	if primaryModel != "" {
		item := m.modelPickerFavoriteItem(all, provider, primaryModel, "primary")
		item.Group = "Current"
		favorites = append(favorites, item)
	}
	if fastModel != "" {
		item := m.modelPickerFavoriteItem(all, provider, fastModel, "fast")
		item.Group = "Current"
		favorites = append(favorites, item)
	}
	return favorites
}

func manualModelPickerItem() pickerItem {
	return pickerItem{
		Label:       "Enter model ID…",
		Value:       "",
		Detail:      "Use a model name not returned by provider discovery",
		Group:       "Manual",
		ManualModel: true,
		Search: pickerSearchIndex(
			"Enter model ID",
			"manual model custom model name",
			"provider discovery",
			"Manual",
			nil,
		),
	}
}

func (m Model) modelPickerItemsForCatalog(cfg *config.Config, favorites, all []pickerItem) []pickerItem {
	favorites = clonePickerItems(favorites)
	if cfg != nil && strings.TrimSpace(cfg.Provider) != "" {
		favorites = append(favorites, manualModelPickerItem())
	}
	catalog := m.modelPickerCatalogItems(all, favorites)
	return append(favorites, catalog...)
}

func (m Model) modelPickerCatalogItems(all, favorites []pickerItem) []pickerItem {
	if len(all) == 0 {
		return nil
	}

	catalog := make([]pickerItem, 0, len(all))
	seen := make(map[string]struct{}, len(favorites))
	for _, item := range favorites {
		if item.Value == "" {
			continue
		}
		key := pickerModelKey(item.Provider, item.Value)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
	}
	for _, item := range all {
		if item.Value == "" {
			continue
		}
		key := pickerModelKey(item.Provider, item.Value)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		catalog = append(catalog, item)
	}
	return catalog
}

func (m Model) modelPickerFavoriteItem(all []pickerItem, provider, model, slot string) pickerItem {
	if item, ok := pickerItemByModel(all, provider, model); ok {
		if item.Detail == "" && item.Metrics == nil {
			item.Detail = slot
		}
		item.Search = append(
			item.Search,
			pickerSearchField{value: slot, weight: 8},
			pickerSearchField{value: "selected", weight: 8},
		)
		return item
	}
	return pickerItem{
		Label:    model,
		Value:    model,
		Provider: provider,
		Detail:   slot,
		Tone:     pickerToneWarn,
		Search: pickerSearchIndex(
			model,
			model,
			slot,
			"Current",
			nil,
		),
	}
}

func (m Model) startupPickerCmd() tea.Cmd {
	overlay := m.Picker.Overlay
	if overlay != nil &&
		overlay.purpose == pickerPurposeModel &&
		overlay.loading &&
		overlay.request != 0 &&
		overlay.cfg != nil {
		if overlay.loadContext == nil {
			overlay.loadContext, overlay.loadCancel = context.WithCancel(context.Background())
		}
		if overlay.setup {
			return checkModelPickerSetup(
				overlay.request,
				overlay.cfg,
				overlay.Preset(),
				overlay.loadContext,
				m.Model.EndpointResolver,
			)
		}
		return loadAllModelPickerItems(
			overlay.request,
			overlay.cfg,
			overlay.Preset(),
			overlay.loadContext,
			m.Model.Catalog,
		)
	}
	if overlay != nil &&
		overlay.purpose == pickerPurposeProviderSetup &&
		overlay.loading &&
		overlay.request != 0 &&
		overlay.cfg != nil {
		return loadProviderPickerItems(
			m.runtimeRequestOperationContext(),
			m.Model.EventGeneration,
			overlay.request,
			*overlay.cfg,
			m.Model.EndpointResolver,
		)
	}

	if sessionPicker := m.Picker.Session; sessionPicker != nil &&
		sessionPicker.loading &&
		sessionPicker.request != 0 &&
		m.Model.SessionCatalog != nil {
		return loadSessionPickerItems(
			sessionPicker.request,
			m.Model.SessionCatalog,
			m.App.Workdir,
			m.runtimeOperationContext(),
		)
	}

	return nil
}

func checkModelPickerSetup(
	requestID uint64,
	cfg *config.Config,
	preset Preset,
	loadContext context.Context,
	resolver *llm.EndpointResolver,
) tea.Cmd {
	cfgCopy := config.Config{}
	if cfg != nil {
		cfgCopy = *cfg
	}
	if loadContext == nil {
		loadContext = context.Background()
	}
	return func() tea.Msg {
		setup, err := providerSetupPrompt(loadContext, &cfgCopy, resolver)
		return modelPickerSetupResolvedMsg{
			requestID: requestID,
			cfg:       cfgCopy,
			preset:    preset,
			setup:     setup,
			err:       err,
		}
	}
}

func checkProviderSetup(
	generation, requestID uint64,
	cfg *config.Config,
	preset Preset,
	loadContext context.Context,
	resolver *llm.EndpointResolver,
) tea.Cmd {
	cfgCopy := config.Config{}
	if cfg != nil {
		cfgCopy = *cfg
	}
	if loadContext == nil {
		loadContext = context.Background()
	}
	return func() tea.Msg {
		setup, err := providerSetupPrompt(loadContext, &cfgCopy, resolver)
		if err == nil && loadContext.Err() != nil {
			err = loadContext.Err()
		}
		return providerSetupResolvedMsg{
			generation: generation,
			requestID:  requestID,
			cfg:        cfgCopy,
			preset:     preset,
			setup:      setup,
			err:        err,
		}
	}
}

func (m Model) handleModelPickerSetupResolved(
	msg modelPickerSetupResolvedMsg,
) (Model, tea.Cmd) {
	if !m.pickerReducer().modelSetupRequestMatches(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		m.pickerReducer().failModelSetup(msg.requestID, msg.err.Error())
		return m, nil
	}
	cfg := msg.cfg
	switch msg.setup {
	case SetupPromptAPIKey:
		return m.openAPIKeyPrompt(&cfg, cfg.Provider, msg.preset)
	case SetupPromptEndpoint:
		return m.openEndpointPrompt(&cfg, msg.preset)
	default:
		return m.openModelSelectionForPreset(&cfg, msg.preset)
	}
}

func (m Model) openModelSelectionForPreset(cfg *config.Config, preset Preset) (Model, tea.Cmd) {
	if cfg != nil {
		if def, ok := llm.LookupConfig(cfg, cfg.Provider); ok && !def.SupportsModelListing {
			return m.openModelIDPrompt(cfg, preset)
		}
	}
	return m.openModelPickerForPreset(cfg, preset)
}

func (m Model) handleModelPickerLoaded(msg modelPickerLoadedMsg) (Model, tea.Cmd) {
	// This handles single-provider model loads (used by setup flow)
	if !m.pickerReducer().modelLoadRequestMatches(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		m.pickerReducer().failModelLoad(
			msg.requestID,
			fmt.Sprintf("Failed to list models for %s: %v", msg.cfg.Provider, msg.err),
		)
		return m, nil
	}
	if len(msg.items) == 0 {
		m.pickerReducer().failModelLoad(
			msg.requestID,
			fmt.Sprintf("No models available for provider %s", msg.cfg.Provider),
		)
		return m, nil
	}

	cfg := &msg.cfg
	favorites := m.modelPickerFavoriteItems(cfg, msg.items)
	combined := m.modelPickerItemsForCatalog(cfg, favorites, msg.items)
	m.pickerReducer().completeModelLoad(
		msg.requestID,
		combined,
		configuredModelForPreset(cfg, msg.preset),
		"",
	)
	return m, nil
}

func togglePreset(p Preset) Preset {
	if p == PresetFast {
		return PresetPrimary
	}
	return PresetFast
}

func normalizeThinkingValue(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", config.DefaultReasoningEffort:
		return config.DefaultReasoningEffort
	case "off", "none", "disabled":
		return "off"
	case "minimal", "min":
		return "minimal"
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	case "xhigh", "extra-high", "extra_high", "extra high":
		return "xhigh"
	case "max", "maximum":
		return "max"
	default:
		return config.DefaultReasoningEffort
	}
}

func thinkingLevelForRuntime(value string) session.ThinkingLevel {
	normalized := normalizeThinkingValue(value)
	if normalized == config.DefaultReasoningEffort {
		return session.ThinkingAuto
	}
	return session.ThinkingLevel(normalized)
}

func thinkingDisplayName(value string) string {
	switch normalizeThinkingValue(value) {
	case "off":
		return "Off"
	case "minimal":
		return "Minimal"
	case "low":
		return "Low"
	case "medium":
		return "Medium"
	case "high":
		return "High"
	case "xhigh":
		return "XHigh"
	case "max":
		return "Max"
	default:
		return "Auto"
	}
}

func (m Model) handlePickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		if m.Picker.Overlay != nil &&
			m.Picker.Overlay.purpose == pickerPurposeProviderSetup &&
			m.Picker.Overlay.loading {
			m.runtimeRequest().clear()
		}
		m.pickerReducer().closeOverlay()
		return m, nil
	case "backspace":
		m.pickerReducer().backspaceOverlayQuery()
		return m, nil
	case "tab":
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			// Tab: autocomplete model name from filtered list
			return m.autocompletePickerModel()
		}
		return m, nil
	case "ctrl+l":
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			preset := togglePreset(m.Picker.Overlay.Preset())
			return m.openModelPickerForPreset(m.Picker.Overlay.cfg, preset)
		}
		return m, nil
	case "pgup", "pageup":
		m.pickerReducer().pageOverlaySelection(-1)
		return m, nil
	case "pgdown", "pagedown":
		m.pickerReducer().pageOverlaySelection(1)
		return m, nil
	case "up":
		m.pickerReducer().moveOverlaySelection(-1)
		return m, nil
	case "down":
		m.pickerReducer().moveOverlaySelection(1)
		return m, nil
	case "enter":
		return m.commitPickerSelection()
	default:
		if text, ok := keyTextInput(msg); ok {
			m.pickerReducer().appendOverlayQuery(text)
			return m, nil
		}
		return m, nil
	}
}

func (m Model) handlePickerPaste(msg tea.PasteMsg) (Model, tea.Cmd) {
	if m.Picker.Overlay == nil {
		return m, nil
	}
	content := inlinePasteText(msg.Content)
	if content == "" {
		return m, nil
	}
	m.pickerReducer().appendOverlayQuery(content)
	return m, nil
}

// autocompletePickerModel completes the current query to the longest common
// prefix of matching model names, shell-style tab completion.
func (m Model) autocompletePickerModel() (Model, tea.Cmd) {
	overlay := m.Picker.Overlay
	if overlay == nil {
		return m, nil
	}
	items := overlay.filtered
	if len(items) == 0 {
		return m, nil
	}
	query := strings.ToLower(overlay.query)
	if query == "" {
		return m, nil
	}
	// Find candidates whose label starts with the query
	var candidates []string
	for _, item := range items {
		if strings.HasPrefix(strings.ToLower(item.Label), query) {
			candidates = append(candidates, item.Label)
		}
	}
	if len(candidates) == 0 {
		return m, nil
	}
	// Longest common prefix of all candidates
	common := candidates[0]
	for _, c := range candidates[1:] {
		common = longestCommonPrefix(common, c)
	}
	if strings.ToLower(common) <= query {
		// Already at max prefix — cycle to next matching item
		for i, item := range items {
			if strings.ToLower(item.Label) == query {
				m.Picker.Overlay.index = (i + 1) % len(items)
				m.pickerReducer().setOverlayQuery(items[(i+1)%len(items)].Label)
				return m, nil
			}
		}
		return m, nil
	}
	m.pickerReducer().setOverlayQuery(common)
	m.Picker.Overlay.index = 0
	return m, nil
}

func longestCommonPrefix(a, b string) string {
	end := min(len(a), len(b))
	for i := 0; i < end; i++ {
		if a[i] != b[i] {
			return a[:i]
		}
	}
	return a[:end]
}

// openProviderSetupPicker shows a provider list for setup/login purposes only.
// This is accessed via Tab from the model picker.
func (m Model) openProviderSetupPicker() (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing runtime settings"))
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	if cfg == nil {
		cfg = &config.Config{}
	}
	requestID := m.runtimeRequest().begin("Loading providers...")
	generation := m.Model.EventGeneration
	ctx := m.runtimeRequestOperationContext()
	cfgCopy := *cfg
	m.clearProgressError()
	m.pickerReducer().openOverlayInvalidatingModelLoads(pickerOverlayState{
		title:    "Provider setup",
		items:    nil,
		filtered: nil,
		index:    0,
		purpose:  pickerPurposeProviderSetup,
		preset:   m.activePreset(),
		cfg:      &cfgCopy,
		loading:  true,
		request:  requestID,
	})
	return m, loadProviderPickerItems(
		ctx,
		generation,
		requestID,
		cfgCopy,
		m.Model.EndpointResolver,
	)
}

func loadProviderPickerItems(
	ctx context.Context,
	generation, requestID uint64,
	cfg config.Config,
	resolver *llm.EndpointResolver,
) tea.Cmd {
	return func() tea.Msg {
		if err := ctx.Err(); err != nil {
			return providerItemsLoadedMsg{
				generation: generation,
				requestID:  requestID,
				cfg:        cfg,
				err:        err,
			}
		}
		items := providerItems(&cfg, resolver)
		if err := ctx.Err(); err != nil {
			return providerItemsLoadedMsg{
				generation: generation,
				requestID:  requestID,
				cfg:        cfg,
				err:        err,
			}
		}
		return providerItemsLoadedMsg{
			generation: generation,
			requestID:  requestID,
			cfg:        cfg,
			items:      items,
		}
	}
}

func (m Model) handleProviderItemsLoaded(msg providerItemsLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration ||
		m.Picker.Overlay == nil ||
		m.Picker.Overlay.purpose != pickerPurposeProviderSetup ||
		!m.Picker.Overlay.loading ||
		m.Picker.Overlay.request != msg.requestID ||
		!m.runtimeRequest().matches(msg.requestID) {
		return m, nil
	}
	if !m.runtimeRequest().finish(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		if errors.Is(msg.err, context.Canceled) {
			return m, nil
		}
		return m.handleLocalError(fmt.Errorf("load providers: %w", msg.err))
	}
	cfg := msg.cfg
	m.clearProgressError()
	m.pickerReducer().openOverlayInvalidatingModelLoads(pickerOverlayState{
		title:    "Provider setup",
		items:    msg.items,
		filtered: append([]pickerItem(nil), msg.items...),
		index:    pickerIndex(msg.items, cfg.Provider),
		purpose:  pickerPurposeProviderSetup,
		preset:   m.activePreset(),
		cfg:      &cfg,
	})
	return m, nil
}

func (m Model) commitPickerSelection() (Model, tea.Cmd) {
	if m.Picker.Overlay == nil {
		return m, nil
	}
	items := pickerDisplayItems(m.Picker.Overlay)
	if len(items) == 0 {
		if m.Picker.Overlay.purpose == pickerPurposeProviderSetup && m.Picker.Overlay.loading {
			m.runtimeRequest().clear()
		}
		m.pickerReducer().closeOverlay()
		return m, nil
	}

	selected := items[m.Picker.Overlay.index]
	var cfg config.Config
	if m.Picker.Overlay.cfg != nil {
		cfg = *m.Picker.Overlay.cfg
	}
	if m.localCommandBusy() && pickerSelectionRequiresIdle(m.Picker.Overlay.purpose) {
		m.pickerReducer().closeOverlay()
		return m, cmdError(m.localCommandBusyMessage("changing runtime settings"))
	}

	switch m.Picker.Overlay.purpose {
	case pickerPurposeModel:
		if selected.ManualModel {
			return m.openModelIDPrompt(&cfg, m.Picker.Overlay.Preset())
		}
		return m.commitUnifiedModelSelection(&cfg, selected)

	case pickerPurposeProviderSetup:
		// Route every provider through the same setup decision so local and
		// endpoint-backed providers do not get an API-key-only prompt.
		return m.handleProviderCommand(selected.Value)

	case pickerPurposeThinking:
		level := normalizeThinkingValue(selected.Value)
		currentCfg, err := m.runtimeConfigForActivePreset(&cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" &&
			normalizeThinkingValue(currentCfg.ReasoningEffort) == level {
			m.pickerReducer().closeOverlay()
			return m, nil
		}
		transition, _, err := m.thinkingSelectionTransition(&cfg, m.activePreset(), level)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		m.pickerReducer().closeOverlay()
		return m.beginRuntimeTransitionCommit(
			transition,
			systemEntry("Thinking set to "+thinkingDisplayName(level)),
		)
	case pickerPurposeSettings:
		fields := strings.Fields(selected.Value)
		if len(fields) != 2 {
			m.pickerReducer().closeOverlay()
			return m, cmdError("invalid settings selection")
		}
		return m.handleSettingsCommand([]string{"/settings", fields[0], fields[1]})
	case pickerPurposeCommand:
		cmd := m.setComposerDraft(selected.Value + " ")
		m.pickerReducer().closeOverlay()
		return m, cmd
	case pickerPurposeHistory:
		cmd := m.setComposerDraft(selected.Value)
		m.pickerReducer().closeOverlay()
		return m, cmd
	default:
		m.pickerReducer().closeOverlay()
		return m, nil
	}
}

// commitUnifiedModelSelection handles model selection from the unified picker.
// Sets both provider and model in one action.
func (m Model) commitUnifiedModelSelection(cfg *config.Config, selected pickerItem) (Model, tea.Cmd) {
	preset := m.Picker.Overlay.Preset()

	// If the selected item needs setup, redirect to login
	if selected.NeedsSetup {
		m.pickerReducer().closeOverlay()
		return m.openAPIKeyPrompt(
			cfgForProvider(cfg, selected.Provider),
			selected.Provider,
			preset,
		)
	}

	// Determine the provider to use
	provider := selected.Provider
	if provider == "" {
		provider = cfg.Provider // fallback to current
	}

	// Check if we need to switch providers
	needProviderChange := !strings.EqualFold(llm.ResolveID(cfg.Provider), llm.ResolveID(provider))

	// Build updated config with both provider and model
	updated := *cfg
	if needProviderChange {
		updated.Provider = provider
		// Clear model fields when switching providers
		updated.Model = ""
		updated.FastModel = ""
		updated.FastReasoningEffort = ""
		updated.SummaryModel = ""
		updated.SummaryReasoningEffort = ""
	}

	// Check if model is already selected (no-op)
	currentCfg, err := m.runtimeConfigForPreset(&updated, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	if !needProviderChange &&
		preset == m.activePreset() &&
		currentCfg.Provider != "" &&
		strings.EqualFold(
			strings.TrimSpace(currentCfg.Model),
			strings.TrimSpace(selected.Value),
		) {
		m.pickerReducer().closeOverlay()
		return m, nil
	}

	// Apply model selection
	transition, _, err := m.modelSelectionTransition(&updated, preset, selected.Value)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	m.pickerReducer().closeOverlay()
	notice := systemEntry("Model set to " + selected.Value)
	if needProviderChange {
		notice = systemEntry("Switched to " + providerDisplayName(provider) + " — " + selected.Value)
	}
	return m.switchRuntimeCommand(
		transition,
		notice,
		m.currentResumeLeafID(),
		false,
	)
}

func (p *pickerOverlayState) Preset() Preset {
	if p == nil {
		return PresetPrimary
	}
	switch p.preset {
	case PresetFast:
		return PresetFast
	default:
		return PresetPrimary
	}
}

// handleProviderCommand handles /provider <name> for direct provider switching.
func (m Model) handleProviderCommand(name string) (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing runtime settings"))
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	updated, err := updateProviderSelection(cfg, name)
	if err != nil {
		return m, cmdError(err.Error())
	}
	preset := m.activePreset()
	requestID := m.runtimeRequest().begin("Checking provider setup...")
	generation := m.Model.EventGeneration
	loadContext := m.runtimeRequestOperationContext()
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeProviderSetup {
		m.pickerReducer().closeOverlay()
	}
	return m, checkProviderSetup(
		generation,
		requestID,
		updated,
		preset,
		loadContext,
		m.Model.EndpointResolver,
	)
}

func (m Model) handleProviderSetupResolved(msg providerSetupResolvedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || !m.runtimeRequest().matches(msg.requestID) {
		return m, nil
	}
	if !m.runtimeRequest().finish(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		if errors.Is(msg.err, context.Canceled) {
			return m, nil
		}
		return m.handleLocalError(msg.err)
	}
	return m.openProviderSelection(&msg.cfg, msg.preset, msg.setup)
}

func (m Model) openProviderSelection(
	cfg *config.Config,
	preset Preset,
	setup SetupPromptKind,
) (Model, tea.Cmd) {
	switch setup {
	case SetupPromptAPIKey:
		return m.openAPIKeyPrompt(cfg, cfg.Provider, preset)
	case SetupPromptEndpoint:
		return m.openEndpointPrompt(cfg, preset)
	}

	// Provider setup has already been resolved above. Catalog discovery is
	// optional and asynchronous; a listing failure must not be misreported as
	// missing credentials.
	def, _ := llm.LookupConfig(cfg, cfg.Provider)

	// Provider ready — open a manual model prompt when the provider does not
	// expose discovery; otherwise use the catalog-backed picker.
	if !def.SupportsModelListing {
		return m.openModelIDPrompt(cfg, preset)
	}
	return m.openModelPickerForPreset(cfg, preset)
}
