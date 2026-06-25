package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/internal/core"
	tea "charm.land/bubbletea/v2"
)

func pickerSelectionRequiresIdle(purpose pickerPurpose) bool {
	switch purpose {
	case pickerPurposeModel, pickerPurposeThinking:
		return true
	default:
		return false
	}
}

func ensureProviderReadyForSelection(ctx context.Context, cfg *config.Config) error {
	if cfg == nil || !llm.IsOpenAICompatible(cfg.Provider) {
		return nil
	}
	if _, ready := llm.ProbeLocalAPIFresh(ctx, cfg); ready {
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
	preset core.Preset,
) (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing runtime settings"))
	}
	if cfg == nil {
		cfg = &config.Config{}
	}

	// Build initial items from favorites + any cached models
	favorites := m.modelPickerFavoriteItems(cfg, nil)
	items := clonePickerItems(favorites)

	m.clearProgressError()
	requestID := m.pickerReducer().beginModelOverlayLoad(pickerOverlayState{
		title:    "Pick a model",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, configuredModelForPreset(cfg, preset)),
		purpose:  pickerPurposeModel,
		preset:   preset,
		cfg:      cfg,
		loading:  true,
	})
	return m, loadAllModelPickerItems(requestID, cfg, preset)
}

// loadAllModelPickerItems loads models from ALL providers that have API keys,
// in parallel. Returns a unified list grouped by provider.
func loadAllModelPickerItems(requestID uint64, cfg *config.Config, preset core.Preset) tea.Cmd {
	cfgCopy := config.Config{}
	if cfg != nil {
		cfgCopy = *cfg
	}
	return func() tea.Msg {
		items := loadAllModelsParallel(context.Background(), &cfgCopy)
		return allModelsLoadedMsg{
			requestID: requestID,
			items:     items,
		}
	}
}

// loadAllModelsParallel fetches models from all providers with API keys concurrently.
func loadAllModelsParallel(ctx context.Context, cfg *config.Config) []pickerItem {
	providers := allProvidersWithAuth(cfg)
	if len(providers) == 0 {
		return nil
	}

	type result struct {
		items []pickerItem
	}
	results := make([]result, len(providers))

	var wg sync.WaitGroup
	for i, prov := range providers {
		wg.Add(1)
		go func(idx int, providerCfg *config.Config) {
			defer wg.Done()
			models, err := listModelsForConfig(ctx, providerCfg)
			if err != nil {
				return // skip providers that fail
			}
			displayName := providerDisplayName(providerCfg.Provider)
			items := modelItemsFromMetadata(models)
			for j := range items {
				items[j].Provider = providerCfg.Provider
				items[j].Group = displayName
			}
			results[idx].items = items
		}(i, prov)
	}
	wg.Wait()

	// Merge results preserving provider order
	var all []pickerItem
	for _, r := range results {
		all = append(all, r.items...)
	}
	return all
}

// allProvidersWithAuth returns configs for all providers that have API keys or are local.
func allProvidersWithAuth(cfg *config.Config) []*config.Config {
	var providers []*config.Config
	for _, def := range llm.Native() {
		if !llm.ShowInPicker(cfg, def) {
			continue
		}
		if !def.SupportsModelListing {
			continue
		}
		provCfg := cfgForProvider(cfg, def.ID)
		// Include if: has API key, or is local, or is openai-compatible with endpoint
		if providerReadyForModelListing(provCfg, def) {
			providers = append(providers, provCfg)
		}
	}
	return providers
}

// providerReadyForModelListing checks if a provider can list models.
func providerReadyForModelListing(cfg *config.Config, def llm.Definition) bool {
	if def.Kind == llm.KindLocal {
		return true
	}
	if llm.IsOpenAICompatible(def.ID) {
		return strings.TrimSpace(cfg.Endpoint) != ""
	}
	return llm.RequiresAuth(cfg, def) == false || llm.ResolvedAuthToken(cfg, def) != ""
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
	)
	return m, nil
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
		{Label: "Off", Value: "off"},
		{Label: "Minimal", Value: "minimal"},
		{Label: "Low", Value: "low"},
		{Label: "Medium", Value: "medium"},
		{Label: "High", Value: "high"},
		{Label: "XHigh", Value: "xhigh"},
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
	switch {
	case primaryModel == "" && fastModel == "":
		return nil
	case primaryModel != "" && strings.EqualFold(primaryModel, fastModel):
		item := m.modelPickerFavoriteItem(all, primaryModel, "primary")
		item.Group = "Current"
		return []pickerItem{item}
	}

	favorites := make([]pickerItem, 0, 2)
	if primaryModel != "" {
		item := m.modelPickerFavoriteItem(all, primaryModel, "primary")
		item.Group = "Current"
		favorites = append(favorites, item)
	}
	if fastModel != "" {
		item := m.modelPickerFavoriteItem(all, fastModel, "fast")
		item.Group = "Current"
		favorites = append(favorites, item)
	}
	return favorites
}

func (m Model) modelPickerItemsForCatalog(cfg *config.Config, favorites, all []pickerItem) []pickerItem {
	catalog := m.modelPickerCatalogItems(all, favorites)
	return append(clonePickerItems(favorites), catalog...)
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
		key := strings.ToLower(item.Value)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
	}
	for _, item := range all {
		if item.Value == "" {
			continue
		}
		key := strings.ToLower(item.Value)
		if _, ok := seen[key]; ok {
			continue
		}
		catalog = append(catalog, item)
	}
	return catalog
}

func (m Model) modelPickerFavoriteItem(all []pickerItem, model, slot string) pickerItem {
	if item, ok := pickerItemByValue(all, model); ok {
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
		Label:  model,
		Value:  model,
		Detail: slot,
		Tone:   pickerToneWarn,
		Search: pickerSearchIndex(
			model,
			model,
			slot,
			"Current",
			nil,
		),
	}
}

func loadModelPickerItems(requestID uint64, cfg *config.Config, preset core.Preset) tea.Cmd {
	cfgCopy := config.Config{}
	if cfg != nil {
		cfgCopy = *cfg
	}
	return func() tea.Msg {
		items, err := modelItemsForProvider(context.Background(), &cfgCopy)
		return modelPickerLoadedMsg{
			requestID: requestID,
			cfg:       cfgCopy,
			preset:    preset,
			items:     items,
			err:       err,
		}
	}
}

func (m Model) startupPickerCmd() tea.Cmd {
	overlay := m.Picker.Overlay
	if overlay != nil &&
		overlay.purpose == pickerPurposeModel &&
		overlay.loading &&
		overlay.request != 0 &&
		overlay.cfg != nil {
		if overlay.setup {
			return checkModelPickerSetup(overlay.request, overlay.cfg, overlay.Preset())
		}
		return loadAllModelPickerItems(overlay.request, overlay.cfg, overlay.Preset())
	}

	if sessionPicker := m.Picker.Session; sessionPicker != nil &&
		sessionPicker.loading &&
		sessionPicker.request != 0 &&
		m.Model.Store != nil {
		return loadSessionPickerItems(sessionPicker.request, m.Model.Store, m.App.Workdir)
	}

	return nil
}

func checkModelPickerSetup(requestID uint64, cfg *config.Config, preset core.Preset) tea.Cmd {
	cfgCopy := config.Config{}
	if cfg != nil {
		cfgCopy = *cfg
	}
	return func() tea.Msg {
		setup, err := providerSetupPrompt(context.Background(), &cfgCopy)
		return modelPickerSetupResolvedMsg{
			requestID: requestID,
			cfg:       cfgCopy,
			preset:    preset,
			setup:     setup,
			err:       err,
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
	case core.SetupPromptAPIKey:
		return m.openAPIKeyPrompt(&cfg, cfg.Provider, msg.preset)
	case core.SetupPromptEndpoint:
		return m.openEndpointPrompt(&cfg, msg.preset)
	default:
		return m.openModelPickerForPreset(&cfg, msg.preset)
	}
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
	)
	return m, nil
}

func togglePreset(p core.Preset) core.Preset {
	if p == presetFast {
		return presetPrimary
	}
	return presetFast
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
	items := providerItems(cfg)
	m.clearProgressError()
	m.pickerReducer().openOverlayInvalidatingModelLoads(pickerOverlayState{
		title:    "Provider setup",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Provider),
		purpose:  pickerPurposeProviderSetup,
		preset:   m.activePreset(),
		cfg:      cfg,
	})
	return m, nil
}

func (m Model) commitPickerSelection() (Model, tea.Cmd) {
	if m.Picker.Overlay == nil {
		return m, nil
	}
	items := pickerDisplayItems(m.Picker.Overlay)
	if len(items) == 0 {
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
		return m.commitUnifiedModelSelection(&cfg, selected)

	case pickerPurposeProviderSetup:
		// Provider setup: open API key prompt for selected provider
		return m.openAPIKeyPrompt(cfgForProvider(&cfg, selected.Value), selected.Value, m.Picker.Overlay.Preset())

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
		m.currentMaterializedSessionID(),
		false,
	)
}

func (p *pickerOverlayState) Preset() core.Preset {
	if p == nil {
		return presetPrimary
	}
	switch p.preset {
	case presetFast:
		return presetFast
	default:
		return presetPrimary
	}
}

func providerModelEntryNotice(provider string) string {
	display := providerDisplayName(provider)
	if strings.TrimSpace(display) == "" {
		display = provider
	}
	return display + " does not provide a model list. Set a model with /model <id>."
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

	// Check if provider needs setup
	def, _ := llm.Lookup(llm.ResolveID(name))
	if def.SupportsModelListing {
		// Try to load models — if auth fails, prompt for API key
		_, err := listModelsForConfig(context.Background(), updated)
		if err != nil {
			return m.openAPIKeyPrompt(cfgForProvider(cfg, name), name, m.activePreset())
		}
	}

	// Provider ready — open model picker for it
	return m.openModelPickerForPreset(updated, m.activePreset())
}

