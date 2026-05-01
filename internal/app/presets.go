package app

import (
	"context"
	"strings"

	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
)

func (p modelPreset) String() string {
	switch p {
	case presetFast:
		return string(presetFast)
	default:
		return string(presetPrimary)
	}
}

func (m Model) activePreset() modelPreset {
	switch m.App.ActivePreset {
	case presetFast:
		return presetFast
	default:
		return presetPrimary
	}
}

func (m Model) activePresetTitle() string {
	switch m.activePreset() {
	case presetFast:
		return "fast"
	default:
		return "primary"
	}
}

func modelPresetFromString(value string) modelPreset {
	if config.NormalizeActivePreset(value) == string(presetFast) {
		return presetFast
	}
	return presetPrimary
}

func (m Model) runtimeConfigForPreset(cfg *config.Config, preset modelPreset) (*config.Config, error) {
	return registry.ResolveRuntimeConfig(context.Background(), cfg, registry.Preset(preset))
}

func (m Model) runtimeConfigForActivePreset(cfg *config.Config) (*config.Config, error) {
	return m.runtimeConfigForPreset(cfg, m.activePreset())
}

func (m Model) commandConfig() (*config.Config, error) {
	if m.Model.Config != nil {
		copied := *m.Model.Config
		return &copied, nil
	}
	return config.Load()
}

func mergeRuntimeSelection(dst, runtime *config.Config) {
	if dst == nil || runtime == nil {
		return
	}
	if strings.TrimSpace(runtime.Provider) != "" {
		dst.Provider = runtime.Provider
		dst.Model = runtime.Model
	}
	if strings.TrimSpace(runtime.ReasoningEffort) != "" {
		dst.ReasoningEffort = runtime.ReasoningEffort
	}
	if strings.TrimSpace(runtime.FastModel) != "" {
		dst.FastModel = runtime.FastModel
	}
	if strings.TrimSpace(runtime.FastReasoningEffort) != "" {
		dst.FastReasoningEffort = runtime.FastReasoningEffort
	}
	if strings.TrimSpace(runtime.SummaryModel) != "" {
		dst.SummaryModel = runtime.SummaryModel
	}
	if strings.TrimSpace(runtime.SummaryReasoningEffort) != "" {
		dst.SummaryReasoningEffort = runtime.SummaryReasoningEffort
	}
}

func (m Model) updateProviderForActivePreset(cfg *config.Config, provider string) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	updated.Provider = providers.ResolveID(provider)
	updated.Model = ""
	updated.FastModel = ""
	updated.SummaryModel = ""
	return &updated
}

func (m Model) updateModelForActivePreset(cfg *config.Config, model string) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	model = strings.TrimSpace(model)
	switch m.activePreset() {
	case presetFast:
		updated.FastModel = model
	default:
		updated.Model = model
	}
	return &updated
}

func (m Model) updateThinkingForActivePreset(cfg *config.Config, effort string) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	effort = strings.TrimSpace(effort)
	switch m.activePreset() {
	case presetFast:
		updated.FastReasoningEffort = effort
	default:
		updated.ReasoningEffort = effort
	}
	return &updated
}

func (m Model) configuredModelForActivePreset(cfg *config.Config) string {
	if cfg == nil {
		return ""
	}
	switch m.activePreset() {
	case presetFast:
		return strings.TrimSpace(cfg.FastModel)
	default:
		return strings.TrimSpace(cfg.Model)
	}
}
