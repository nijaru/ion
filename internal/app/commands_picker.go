package app

import (
	"context"
	"errors"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/providers"
)

func pickerSelectionRequiresIdle(purpose pickerPurpose) bool {
	switch purpose {
	case pickerPurposeProvider, pickerPurposeModel, pickerPurposeThinking:
		return true
	default:
		return false
	}
}

func ensureProviderReadyForSelection(ctx context.Context, cfg *config.Config) error {
	if cfg == nil || !providers.IsOpenAICompatible(cfg.Provider) {
		return nil
	}
	if _, ready := providers.ProbeLocalAPIFresh(ctx, cfg); ready {
		return nil
	}
	if strings.TrimSpace(cfg.Endpoint) != "" {
		return errors.New("OpenAI-compatible endpoint is not running")
	}
	return errors.New("set an endpoint or start a local OpenAI-compatible server")
}

func (m Model) openProviderPicker() (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openProviderPickerWithConfig(cfg)
}

func (m Model) openProviderPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	return m.openProviderPickerForPreset(cfg, m.activePreset())
}

func (m Model) openProviderPickerForPreset(
	cfg *config.Config,
	preset Preset,
) (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing runtime settings"))
	}
	if cfg == nil {
		cfg = &config.Config{}
	}
	items := providerItems(cfg)
	m.clearProgressError()
	m.pickerReducer().openOverlayInvalidatingModelLoads(pickerOverlayState{
		title:    "Pick a provider",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Provider),
		purpose:  pickerPurposeProvider,
		preset:   preset,
		cfg:      cfg,
	})
	return m, nil
}

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
	if cfg.Provider == "" {
		return m.openProviderPickerWithConfig(cfg)
	}
	if !providers.SupportsModelListing(cfg) {
		return m, cmdError(providerModelEntryNotice(cfg.Provider))
	}
	if providers.IsOpenAICompatible(cfg.Provider) {
		return m.beginModelPickerSetupCheck(cfg, preset)
	}
	setup, err := providerSetupPrompt(context.Background(), cfg)
	if err != nil {
		return m, cmdError(err.Error())
	}
	switch setup {
	case setupPromptAPIKey:
		return m.openAPIKeyPrompt(cfg, cfg.Provider, preset)
	case setupPromptEndpoint:
		return m.openEndpointPrompt(cfg, preset)
	}
	return m.openReadyModelPickerForPreset(cfg, preset)
}

func (m Model) beginModelPickerSetupCheck(
	cfg *config.Config,
	preset Preset,
) (Model, tea.Cmd) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	items := m.modelPickerFavoriteItems(cfg, nil)
	m.clearProgressError()
	requestID := m.pickerReducer().beginModelOverlayLoad(pickerOverlayState{
		title: "Pick " + presetTitle(preset) + " model: " + modelPickerProviderTitle(
			cfg.Provider,
		),
		items:    clonePickerItems(items),
		filtered: clonePickerItems(items),
		index:    pickerIndex(items, configuredModelForPreset(cfg, preset)),
		purpose:  pickerPurposeModel,
		preset:   preset,
		cfg:      cfg,
		loading:  true,
		setup:    true,
	})
	return m, checkModelPickerSetup(requestID, cfg, preset)
}

func checkModelPickerSetup(requestID uint64, cfg *config.Config, preset Preset) tea.Cmd {
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
	case setupPromptAPIKey:
		return m.openAPIKeyPrompt(&cfg, cfg.Provider, msg.preset)
	case setupPromptEndpoint:
		return m.openEndpointPrompt(&cfg, msg.preset)
	default:
		return m.openReadyModelPickerForPreset(&cfg, msg.preset)
	}
}

func (m Model) beginProviderSelection(
	cfg *config.Config,
	provider string,
	preset Preset,
) (Model, tea.Cmd) {
	updated, err := updateProviderSelection(cfg, provider)
	if err != nil {
		return m, cmdError(err.Error())
	}

	requestID := m.pickerReducer().beginProviderSelection()
	m.progressReducer().beginLocalStatus("Checking provider...")
	m.pickerReducer().markProviderOverlayLoading(requestID)
	cfgCopy := *updated
	return m, func() tea.Msg {
		selection, err := providerSelectionForConfig(context.Background(), &cfgCopy, preset)
		return providerSelectionResolvedMsg{
			requestID: requestID,
			provider:  cfgCopy.Provider,
			preset:    preset,
			selection: selection,
			err:       err,
		}
	}
}

func (m Model) handleProviderSelectionResolved(
	msg providerSelectionResolvedMsg,
) (Model, tea.Cmd) {
	if !m.pickerReducer().settleProviderSelection(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		if msg.selection.cfg == nil {
			m.pickerReducer().closeOverlay()
		}
		return m.handleLocalError(msg.err)
	}
	return m.applyProviderSelection(msg.selection, msg.provider, msg.preset)
}

func (m Model) applyProviderSelection(
	selection providerSelection,
	provider string,
	preset Preset,
) (Model, tea.Cmd) {
	m.clearProgressError()
	if selection.setup != 0 {
		switch selection.setup {
		case setupPromptAPIKey:
			return m.openAPIKeyPrompt(selection.cfg, provider, preset)
		case setupPromptEndpoint:
			return m.openEndpointPrompt(selection.cfg, preset)
		}
	}

	m.pickerReducer().closeOverlay()
	if !selection.supportsModelListing {
		return m.beginRuntimeTransitionCommit(
			selection.transition,
			systemEntry(providerModelEntryNotice(selection.cfg.Provider)),
		)
	}
	return m.openReadyModelPickerForPreset(selection.cfg, preset)
}

func (m Model) openReadyModelPickerForPreset(
	cfg *config.Config,
	preset Preset,
) (Model, tea.Cmd) {
	cached, fresh, ok := cachedModelItemsForProvider(cfg)
	items := m.modelPickerItemsForCatalog(cfg, cached)
	loading := !fresh
	if !ok {
		items = m.modelPickerFavoriteItems(cfg, nil)
	}
	m.clearProgressError()
	requestID := m.pickerReducer().beginModelOverlayLoad(pickerOverlayState{
		title: "Pick " + presetTitle(preset) + " model: " + modelPickerProviderTitle(
			cfg.Provider,
		),
		items:    clonePickerItems(items),
		filtered: clonePickerItems(items),
		index:    pickerIndex(items, configuredModelForPreset(cfg, preset)),
		purpose:  pickerPurposeModel,
		preset:   preset,
		cfg:      cfg,
		loading:  loading,
	})
	if fresh {
		return m, nil
	}
	return m, loadModelPickerItems(requestID, cfg, preset)
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

func (m Model) modelPickerItemsForCatalog(cfg *config.Config, items []pickerItem) []pickerItem {
	favorites := m.modelPickerFavoriteItems(cfg, items)
	catalog := m.modelPickerCatalogItems(items, favorites)
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
		item.Group = "All models"
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

func modelPickerProviderTitle(provider string) string {
	display := providerDisplayName(provider)
	if strings.TrimSpace(display) != "" {
		return display
	}
	return provider
}

func loadModelPickerItems(requestID uint64, cfg *config.Config, preset Preset) tea.Cmd {
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
		return loadModelPickerItems(overlay.request, overlay.cfg, overlay.Preset())
	}

	if sessionPicker := m.Picker.Session; sessionPicker != nil &&
		sessionPicker.loading &&
		sessionPicker.request != 0 &&
		m.Model.Store != nil {
		return loadSessionPickerItems(sessionPicker.request, m.Model.Store, m.App.Workdir)
	}

	return nil
}

func (m Model) handleModelPickerLoaded(msg modelPickerLoadedMsg) (Model, tea.Cmd) {
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
	combined := m.modelPickerItemsForCatalog(cfg, msg.items)
	m.pickerReducer().completeModelLoad(
		msg.requestID,
		combined,
		configuredModelForPreset(cfg, msg.preset),
	)
	return m, nil
}

func togglePreset(p Preset) Preset {
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
		if m.Picker.Overlay.purpose == pickerPurposeProvider {
			if m.Picker.Overlay.cfg != nil && m.Picker.Overlay.cfg.Provider != "" {
				return m.openModelPickerForPreset(
					m.Picker.Overlay.cfg,
					m.Picker.Overlay.Preset(),
				)
			}
			return m, nil
		}
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			return m.openProviderPickerForPreset(
				m.Picker.Overlay.cfg,
				m.Picker.Overlay.Preset(),
			)
		}
		return m, nil
	case "ctrl+m":
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
	case pickerPurposeProvider:
		preset := m.Picker.Overlay.Preset()
		return m.beginProviderSelection(&cfg, selected.Value, preset)

	case pickerPurposeModel:
		preset := m.Picker.Overlay.Preset()
		currentCfg, err := m.runtimeConfigForPreset(&cfg, preset)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if preset == m.activePreset() &&
			currentCfg.Provider != "" &&
			strings.EqualFold(
				strings.TrimSpace(currentCfg.Model),
				strings.TrimSpace(selected.Value),
			) {
			m.pickerReducer().closeOverlay()
			return m, nil
		}
		transition, _, err := m.modelSelectionTransition(&cfg, preset, selected.Value)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		m.pickerReducer().closeOverlay()
		notice := systemEntry("Model set to " + selected.Value)
		return m.switchRuntimeCommand(
			transition,
			notice,
			m.currentMaterializedSessionID(),
			false,
		)
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

func (p *pickerOverlayState) Preset() Preset {
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
