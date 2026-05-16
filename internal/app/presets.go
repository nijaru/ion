package app

import (
	"context"
	"fmt"
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
	return presetTitle(m.activePreset())
}

func presetTitle(preset modelPreset) string {
	switch preset {
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

func (m Model) runtimeConfigForPreset(
	cfg *config.Config,
	preset modelPreset,
) (*config.Config, error) {
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

func updateProviderSelection(
	cfg *config.Config,
	provider string,
) (*config.Config, error) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	resolved := providers.ResolveID(provider)
	def, ok := providers.Lookup(resolved)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", strings.TrimSpace(provider))
	}
	if def.Runtime != providers.RuntimeNative {
		return nil, fmt.Errorf("ACP providers are deferred until the advanced integration phase")
	}
	updated := *cfg
	updated.Provider = def.ID
	if providers.ResolveID(cfg.Provider) == def.ID {
		return &updated, nil
	}
	updated.Model = ""
	updated.FastModel = ""
	updated.SummaryModel = ""
	return &updated, nil
}

func (m Model) updateModelForActivePreset(cfg *config.Config, model string) *config.Config {
	return updateModelForPreset(cfg, model, m.activePreset())
}

func updateModelForPreset(
	cfg *config.Config,
	model string,
	preset modelPreset,
) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	model = strings.TrimSpace(model)
	switch preset {
	case presetFast:
		updated.FastModel = model
	default:
		updated.Model = model
	}
	return &updated
}

func (m Model) updateThinkingForActivePreset(cfg *config.Config, effort string) *config.Config {
	return updateThinkingForPreset(cfg, effort, m.activePreset())
}

func updateThinkingForPreset(
	cfg *config.Config,
	effort string,
	preset modelPreset,
) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	effort = strings.TrimSpace(effort)
	switch preset {
	case presetFast:
		updated.FastReasoningEffort = effort
	default:
		updated.ReasoningEffort = effort
	}
	return &updated
}

func (m Model) configuredModelForActivePreset(cfg *config.Config) string {
	return configuredModelForPreset(cfg, m.activePreset())
}

func configuredModelForPreset(cfg *config.Config, preset modelPreset) string {
	if cfg == nil {
		return ""
	}
	switch preset {
	case presetFast:
		return strings.TrimSpace(cfg.FastModel)
	default:
		return strings.TrimSpace(cfg.Model)
	}
}
