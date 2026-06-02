package app

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/llm"
)

func (m Model) activePreset() Preset {
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

func presetTitle(preset Preset) string {
	switch preset {
	case presetFast:
		return "fast"
	default:
		return "primary"
	}
}

func (m Model) runtimeConfigForPreset(
	cfg *config.Config,
	preset Preset,
) (*config.Config, error) {
	return llm.ResolveRuntimeConfig(context.Background(), cfg, llm.Preset(preset))
}

func (m Model) runtimeConfigForActivePreset(cfg *config.Config) (*config.Config, error) {
	return m.runtimeConfigForPreset(cfg, m.activePreset())
}

func (m Model) commandConfig() (*config.Config, error) {
	if m.Model.Config != nil {
		copied := *m.Model.Config
		return &copied, nil
	}
	return &config.Config{}, nil
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
	resolved := llm.ResolveID(provider)
	def, ok := llm.Lookup(resolved)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", strings.TrimSpace(provider))
	}
	if def.Runtime != llm.RuntimeNative {
		return nil, fmt.Errorf("ACP providers are deferred until the advanced integration phase")
	}
	updated := *cfg
	updated.Provider = def.ID
	if llm.ResolveID(cfg.Provider) == def.ID {
		return &updated, nil
	}
	updated.Model = ""
	updated.FastModel = ""
	updated.FastReasoningEffort = ""
	updated.SummaryModel = ""
	updated.SummaryReasoningEffort = ""
	return &updated, nil
}

func (m Model) updateModelForActivePreset(cfg *config.Config, model string) *config.Config {
	return updateModelForPreset(cfg, model, m.activePreset())
}

func updateModelForPreset(
	cfg *config.Config,
	model string,
	preset Preset,
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
	preset Preset,
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

func configuredModelForPreset(cfg *config.Config, preset Preset) string {
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
