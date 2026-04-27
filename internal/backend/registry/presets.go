package registry

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/config"
)

type Preset string

const (
	PresetPrimary Preset = "primary"
	PresetFast    Preset = "fast"
	PresetSummary Preset = "summary"
)

func ResolveRuntimeConfig(ctx context.Context, cfg *config.Config, preset Preset) (*config.Config, error) {
	if cfg == nil {
		return &config.Config{}, nil
	}

	out := *cfg
	out.Provider = strings.ToLower(strings.TrimSpace(out.Provider))
	out.Model = strings.TrimSpace(out.Model)

	switch preset {
	case PresetPrimary:
		out.ReasoningEffort = normalizeRequiredReasoning(out.ReasoningEffort, config.DefaultReasoningEffort)
		return &out, nil
	case PresetFast:
		return resolveFastRuntimeConfig(&out, cfg)
	case PresetSummary:
		if strings.TrimSpace(cfg.SummaryModel) == "" && strings.TrimSpace(cfg.SummaryReasoningEffort) == "" {
			if strings.TrimSpace(cfg.FastModel) != "" {
				return resolveFastRuntimeConfig(&out, cfg)
			}
			out.ReasoningEffort = normalizeOptionalReasoning(cfg.SummaryReasoningEffort, "low")
			return &out, nil
		}
		if strings.TrimSpace(cfg.SummaryModel) != "" {
			out.Model = strings.TrimSpace(cfg.SummaryModel)
		}
		out.ReasoningEffort = normalizeOptionalReasoning(cfg.SummaryReasoningEffort, "low")
		return &out, nil
	default:
		return nil, fmt.Errorf("unknown preset %q", preset)
	}
}

func resolveFastRuntimeConfig(out *config.Config, cfg *config.Config) (*config.Config, error) {
	if strings.TrimSpace(cfg.Provider) == "" {
		return nil, fmt.Errorf("provider is required to resolve a fast preset")
	}
	out.Provider = strings.TrimSpace(cfg.Provider)
	if strings.TrimSpace(cfg.FastModel) != "" {
		out.Model = strings.TrimSpace(cfg.FastModel)
	} else {
		return nil, fmt.Errorf("fast model is not configured; open /model and press Ctrl+M to choose one")
	}
	out.ReasoningEffort = normalizeOptionalReasoning(cfg.FastReasoningEffort, "low")
	return out, nil
}

func normalizeOptionalReasoning(value, fallback string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "":
		return fallback
	case "auto":
		return fallback
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
		return fallback
	}
}

func normalizeRequiredReasoning(value, fallback string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "":
		return fallback
	case "auto":
		return fallback
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
		return fallback
	}
}
