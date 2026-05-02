package app

import (
	"context"
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

func (m Model) openProviderPicker() (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openProviderPickerWithConfig(cfg)
}

func (m Model) openProviderPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	items := providerItems(cfg)
	m.clearProgressError()
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Provider),
		purpose:  pickerPurposeProvider,
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
	if cfg.Provider == "" {
		return m.openProviderPickerWithConfig(cfg)
	}
	if !providers.SupportsModelListing(cfg) {
		return m, cmdError(providerModelEntryNotice(cfg.Provider))
	}
	items, err := modelItemsForProvider(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to list models for %s: %v", cfg.Provider, err))
	}
	if len(items) == 0 {
		return m, cmdError(fmt.Sprintf("no models available for provider %s", cfg.Provider))
	}
	favorites := m.modelPickerFavoriteItems(cfg, items)
	catalog := m.modelPickerCatalogItems(items, favorites)
	combined := append(clonePickerItems(favorites), catalog...)
	m.clearProgressError()
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a " + m.activePresetTitle() + " model for " + cfg.Provider,
		items:    combined,
		filtered: clonePickerItems(combined),
		index:    pickerIndex(combined, m.configuredModelForActivePreset(cfg)),
		purpose:  pickerPurposeModel,
		cfg:      cfg,
	}
	return m, nil
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
		item := m.modelPickerFavoriteItem(all, primaryModel)
		item.Group = "Configured presets"
		return []pickerItem{item}
	}

	favorites := make([]pickerItem, 0, 2)
	if primaryModel != "" {
		item := m.modelPickerFavoriteItem(all, primaryModel)
		item.Group = "Configured presets"
		favorites = append(favorites, item)
	}
	if fastModel != "" {
		item := m.modelPickerFavoriteItem(all, fastModel)
		item.Group = "Configured presets"
		favorites = append(favorites, item)
	}
	return favorites
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

func (m Model) modelPickerFavoriteItem(all []pickerItem, model string) pickerItem {
	if item, ok := pickerItemByValue(all, model); ok {
		return item
	}
	return pickerItem{
		Label:   model,
		Value:   model,
		Detail:  "metadata unavailable",
		Tone:    pickerToneWarn,
		Metrics: &pickerMetrics{Context: "—", Input: "—", Output: "—"},
		Search: pickerSearchIndex(
			model,
			model,
			"metadata unavailable",
			"Configured presets",
			&pickerMetrics{Context: "—", Input: "—", Output: "—"},
		),
	}
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
				runtimeCfg, err := m.runtimeConfigForActivePreset(m.Picker.Overlay.cfg)
				if err != nil {
					return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
				}
				return m.openModelPickerWithConfig(runtimeCfg)
			}
			return m, nil
		}
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			return m.openProviderPickerWithConfig(m.Picker.Overlay.cfg)
		}
		return m, nil
	case "ctrl+m":
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			m.App.ActivePreset = togglePreset(m.activePreset())
			if err := config.SaveActivePreset(m.App.ActivePreset.String()); err != nil {
				return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
			}
			return m.openModelPickerWithConfig(m.Picker.Overlay.cfg)
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
		if def, ok := providers.Lookup(selected.Value); ok && def.ID == "local-api" {
			if _, ready := providers.CredentialStateContext(context.Background(), cfgForProvider(&cfg, def.ID), def); !ready {
				return m, cmdError("Local API is not running")
			}
		}
		if strings.EqualFold(cfg.Provider, selected.Value) {
			m.Picker.Overlay = nil
			return m.openModelPickerWithConfig(&cfg)
		}
		updated := m.updateProviderForActivePreset(&cfg, selected.Value)
		if err := config.SaveState(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Model.Backend.SetConfig(updated)
		m.Model.Config = updated
		m.clearProgressError()
		m.Progress.Status = noModelConfiguredStatus()
		m.Picker.Overlay = nil
		if !providers.SupportsModelListing(updated) {
			return m, m.printEntries(session.Entry{
				Role:    session.System,
				Content: providerModelEntryNotice(updated.Provider),
			})
		}
		return m.openModelPickerWithConfig(updated)

	case pickerPurposeModel:
		currentCfg, err := m.runtimeConfigForActivePreset(&cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" &&
			strings.EqualFold(
				strings.TrimSpace(currentCfg.Model),
				strings.TrimSpace(selected.Value),
			) {
			m.Picker.Overlay = nil
			return m, nil
		}
		updated := m.updateModelForActivePreset(&cfg, selected.Value)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.SaveState(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Picker.Overlay = nil
		notice := session.Entry{Role: session.System, Content: "Model set to " + selected.Value}
		return m, m.switchRuntimeCommand(
			runtimeCfg,
			updated,
			m.activePreset(),
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
		if err := config.SaveReasoningState(m.activePreset().String(), level); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Model.Backend.SetConfig(runtimeCfg)
		m.Progress.ReasoningEffort = level
		m.Picker.Overlay = nil
		return m, m.printEntries(
			session.Entry{
				Role:    session.System,
				Content: "Thinking set to " + thinkingDisplayName(level),
			},
		)
	case pickerPurposeCommand:
		m.Input.Composer.SetValue(selected.Value + " ")
		m.relayoutComposer()
		m.Picker.Overlay = nil
		return m, nil
	default:
		m.Picker.Overlay = nil
		return m, nil
	}
}

func providerModelEntryNotice(provider string) string {
	display := providerDisplayName(provider)
	if strings.TrimSpace(display) == "" {
		display = provider
	}
	return display + " does not provide a model list. Set a model with /model <id>."
}
