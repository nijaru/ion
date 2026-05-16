package app

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/session"
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
	if cfg == nil || providers.ResolveID(cfg.Provider) != "local-api" {
		return nil
	}
	if _, ready := providers.ProbeLocalAPIFresh(ctx, cfg); ready {
		return nil
	}
	return errors.New("Local API is not running")
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
	preset modelPreset,
) (Model, tea.Cmd) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	items := providerItems(cfg)
	m.clearProgressError()
	m.Picker.ModelLoadRequest++
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Provider),
		purpose:  pickerPurposeProvider,
		preset:   preset,
		cfg:      cfg,
	}
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
	preset modelPreset,
) (Model, tea.Cmd) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	if cfg.Provider == "" {
		return m.openProviderPickerWithConfig(cfg)
	}
	if !providers.SupportsModelListing(cfg) {
		return m, cmdError(providerModelEntryNotice(cfg.Provider))
	}
	m.Picker.ModelLoadRequest++
	requestID := m.Picker.ModelLoadRequest
	cached, fresh, ok := cachedModelItemsForProvider(cfg)
	items := m.modelPickerItemsForCatalog(cfg, cached)
	loading := !fresh
	if !ok {
		items = m.modelPickerFavoriteItems(cfg, nil)
	}
	m.clearProgressError()
	m.Picker.Overlay = &pickerOverlayState{
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
		request:  requestID,
	}
	if fresh {
		return m, nil
	}
	return m, loadModelPickerItems(requestID, cfg)
}

func (m Model) openThinkingPicker() (Model, tea.Cmd) {
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
	for i := range items {
		items[i].Search = pickerSearchIndex(
			items[i].Label,
			items[i].Value,
			items[i].Detail,
			"",
			nil,
		)
	}
	m.Picker.ModelLoadRequest++
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a " + m.activePresetTitle() + " thinking level",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, normalizeThinkingValue(runtimeCfg.ReasoningEffort)),
		purpose:  pickerPurposeThinking,
		cfg:      cfg,
	}
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

func loadModelPickerItems(requestID uint64, cfg *config.Config) tea.Cmd {
	cfgCopy := config.Config{}
	if cfg != nil {
		cfgCopy = *cfg
	}
	return func() tea.Msg {
		items, err := modelItemsForProvider(context.Background(), &cfgCopy)
		return modelPickerLoadedMsg{
			requestID: requestID,
			cfg:       cfgCopy,
			items:     items,
			err:       err,
		}
	}
}

func (m Model) handleModelPickerLoaded(msg modelPickerLoadedMsg) (Model, tea.Cmd) {
	overlay := m.Picker.Overlay
	if overlay == nil ||
		overlay.purpose != pickerPurposeModel ||
		overlay.request != msg.requestID ||
		msg.requestID != m.Picker.ModelLoadRequest {
		return m, nil
	}

	overlay.loading = false
	overlay.err = ""
	if msg.err != nil {
		overlay.err = fmt.Sprintf("Failed to list models for %s: %v", msg.cfg.Provider, msg.err)
		if len(overlay.items) == 0 {
			overlay.filtered = nil
		}
		return m, nil
	}
	if len(msg.items) == 0 {
		overlay.err = fmt.Sprintf("No models available for provider %s", msg.cfg.Provider)
		if len(overlay.items) == 0 {
			overlay.filtered = nil
		}
		return m, nil
	}

	cfg := &msg.cfg
	combined := m.modelPickerItemsForCatalog(cfg, msg.items)
	overlay.items = combined
	overlay.filtered = clonePickerItems(combined)
	overlay.index = pickerIndex(combined, configuredModelForPreset(cfg, overlay.modelPreset()))
	if overlay.query != "" {
		refreshPickerFilter(&m)
	}
	return m, nil
}

func togglePreset(p modelPreset) modelPreset {
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
		m.Picker.Overlay = nil
		return m, nil
	case "backspace":
		if len(m.Picker.Overlay.query) > 0 {
			_, size := utf8.DecodeLastRuneInString(m.Picker.Overlay.query)
			m.Picker.Overlay.query = m.Picker.Overlay.query[:len(m.Picker.Overlay.query)-size]
			refreshPickerFilter(&m)
		}
		return m, nil
	case "tab":
		if m.Picker.Overlay.purpose == pickerPurposeProvider {
			if m.Picker.Overlay.cfg != nil && m.Picker.Overlay.cfg.Provider != "" {
				return m.openModelPickerForPreset(
					m.Picker.Overlay.cfg,
					m.Picker.Overlay.modelPreset(),
				)
			}
			return m, nil
		}
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			return m.openProviderPickerForPreset(
				m.Picker.Overlay.cfg,
				m.Picker.Overlay.modelPreset(),
			)
		}
		return m, nil
	case "ctrl+m":
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			preset := togglePreset(m.Picker.Overlay.modelPreset())
			return m.openModelPickerForPreset(m.Picker.Overlay.cfg, preset)
		}
		return m, nil
	case "pgup", "pageup":
		if m.Picker.Overlay.index > 0 {
			m.Picker.Overlay.index -= pickerPageSize
			if m.Picker.Overlay.index < 0 {
				m.Picker.Overlay.index = 0
			}
		}
		return m, nil
	case "pgdown", "pagedown":
		if max := len(pickerDisplayItems(m.Picker.Overlay)); max > 0 {
			m.Picker.Overlay.index += pickerPageSize
			if m.Picker.Overlay.index >= max {
				m.Picker.Overlay.index = max - 1
			}
		}
		return m, nil
	case "up":
		if m.Picker.Overlay.index > 0 {
			m.Picker.Overlay.index--
		}
		return m, nil
	case "down":
		if m.Picker.Overlay.index < len(pickerDisplayItems(m.Picker.Overlay))-1 {
			m.Picker.Overlay.index++
		}
		return m, nil
	case "enter":
		return m.commitPickerSelection()
	default:
		if msg.Text != "" {
			m.Picker.Overlay.query += msg.Text
			refreshPickerFilter(&m)
			return m, nil
		}
		return m, nil
	}
}

func (m Model) commitPickerSelection() (Model, tea.Cmd) {
	if m.Picker.Overlay == nil {
		return m, nil
	}
	items := pickerDisplayItems(m.Picker.Overlay)
	if len(items) == 0 {
		m.Picker.Overlay = nil
		return m, nil
	}

	selected := items[m.Picker.Overlay.index]
	var cfg config.Config
	if m.Picker.Overlay.cfg != nil {
		cfg = *m.Picker.Overlay.cfg
	}
	if m.localCommandBusy() && pickerSelectionRequiresIdle(m.Picker.Overlay.purpose) {
		m.Picker.Overlay = nil
		return m, cmdError("Finish or cancel the current turn before changing runtime settings.")
	}

	switch m.Picker.Overlay.purpose {
	case pickerPurposeProvider:
		preset := m.Picker.Overlay.modelPreset()
		updated := &cfg
		if strings.EqualFold(cfg.Provider, selected.Value) {
			if err := ensureProviderReadyForSelection(context.Background(), updated); err != nil {
				return m, cmdError(err.Error())
			}
			m.Picker.Overlay = nil
			return m.openModelPickerForPreset(updated, preset)
		}
		updated, err := updateProviderSelection(&cfg, selected.Value)
		if err != nil {
			m.Picker.Overlay = nil
			return m, cmdError(err.Error())
		}
		if err := ensureProviderReadyForSelection(context.Background(), updated); err != nil {
			return m, cmdError(err.Error())
		}
		m.clearProgressError()
		m.Picker.Overlay = nil
		if !providers.SupportsModelListing(updated) {
			transition := newRuntimeTransition(
				updated,
				updated,
				m.activePreset(),
				noModelConfiguredStatus(),
			).withStatePersistence()
			var commitErr error
			m, commitErr = m.commitRuntimeTransition(transition)
			if commitErr != nil {
				return m, runtimeTransitionErrorCmd(commitErr)
			}
			return m, m.printEntries(session.Entry{
				Role:    session.System,
				Content: providerModelEntryNotice(updated.Provider),
			})
		}
		return m.openModelPickerForPreset(updated, preset)

	case pickerPurposeModel:
		preset := m.Picker.Overlay.modelPreset()
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
			m.Picker.Overlay = nil
			return m, nil
		}
		updated := updateModelForPreset(&cfg, selected.Value, preset)
		runtimeCfg, err := m.runtimeConfigForPreset(updated, preset)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		m.Picker.Overlay = nil
		notice := session.Entry{Role: session.System, Content: "Model set to " + selected.Value}
		transition := newRuntimeTransition(updated, runtimeCfg, preset, "").
			withStatePersistence()
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
			m.Picker.Overlay = nil
			return m, nil
		}
		updated := m.updateThinkingForActivePreset(&cfg, level)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		transition := newRuntimeTransition(
			updated,
			runtimeCfg,
			m.activePreset(),
			"",
		).withReasoningPersistence(m.activePreset(), level)
		var commitErr error
		m, commitErr = m.commitRuntimeTransition(transition)
		if commitErr != nil {
			return m, runtimeTransitionErrorCmd(commitErr)
		}
		m.Picker.Overlay = nil
		return m, m.printEntries(
			session.Entry{
				Role:    session.System,
				Content: "Thinking set to " + thinkingDisplayName(level),
			},
		)
	case pickerPurposeCommand:
		m.setComposerDraft(selected.Value + " ")
		m.Picker.Overlay = nil
		return m, nil
	default:
		m.Picker.Overlay = nil
		return m, nil
	}
}

func (p *pickerOverlayState) modelPreset() modelPreset {
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
