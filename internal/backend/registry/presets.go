package registry

import (
	"context"
	"fmt"
	"slices"
	"strings"

	"github.com/nijaru/ion/internal/config"
)

type Preset string

const (
	PresetPrimary Preset = "primary"
	PresetFast    Preset = "fast"
	PresetSummary Preset = "summary"
)

var ListModelsForConfigHook = ListModelsForConfig

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
		return resolveFastRuntimeConfig(ctx, &out, cfg)
	case PresetSummary:
		if strings.TrimSpace(cfg.SummaryModel) == "" && strings.TrimSpace(cfg.SummaryReasoningEffort) == "" {
			return resolveFastRuntimeConfig(ctx, &out, cfg)
		}
		if strings.TrimSpace(cfg.SummaryModel) != "" {
			out.Model = strings.TrimSpace(cfg.SummaryModel)
		} else {
			resolved, err := resolveFastModel(ctx, cfg)
			if err != nil {
				return nil, err
			}
			out.Model = resolved
		}
		out.ReasoningEffort = normalizeOptionalReasoning(cfg.SummaryReasoningEffort, "low")
		return &out, nil
	default:
		return nil, fmt.Errorf("unknown preset %q", preset)
	}
}

func resolveFastRuntimeConfig(ctx context.Context, out *config.Config, cfg *config.Config) (*config.Config, error) {
	if strings.TrimSpace(cfg.Provider) == "" {
		return nil, fmt.Errorf("provider is required to resolve a fast preset")
	}
	out.Provider = strings.TrimSpace(cfg.Provider)
	if strings.TrimSpace(cfg.FastModel) != "" {
		out.Model = strings.TrimSpace(cfg.FastModel)
	} else {
		resolved, err := resolveFastModel(ctx, cfg)
		if err != nil {
			return nil, err
		}
		out.Model = resolved
	}
	out.ReasoningEffort = normalizeOptionalReasoning(cfg.FastReasoningEffort, "low")
	return out, nil
}

func resolveFastModel(ctx context.Context, cfg *config.Config) (string, error) {
	if cfg == nil {
		return "", fmt.Errorf("provider is required to resolve a fast model")
	}
	provider := strings.TrimSpace(cfg.Provider)
	if provider == "" {
		return "", fmt.Errorf("provider is required to resolve a fast model")
	}

	models, err := ListModelsForConfigHook(ctx, &config.Config{
		Provider:     provider,
		Endpoint:     strings.TrimSpace(cfg.Endpoint),
		AuthEnvVar:   strings.TrimSpace(cfg.AuthEnvVar),
		ExtraHeaders: cfg.ExtraHeaders,
	})
	if err != nil {
		return "", err
	}
	if len(models) == 0 {
		return "", fmt.Errorf("no models available for provider %s", provider)
	}

	for _, token := range fastModelTokens {
		if matched := chooseMatchingModel(models, token); matched != "" {
			return matched, nil
		}
	}

	slices.SortFunc(models, compareFastModels)
	return models[0].ID, nil
}

func chooseMatchingModel(models []ModelMetadata, token string) string {
	matches := make([]ModelMetadata, 0, len(models))
	needle := strings.ToLower(strings.TrimSpace(token))
	for _, meta := range models {
		if strings.Contains(strings.ToLower(meta.ID), needle) {
			matches = append(matches, meta)
		}
	}
	if len(matches) == 0 {
		return ""
	}
	slices.SortFunc(matches, compareFastModels)
	return matches[0].ID
}

func compareFastModels(a, b ModelMetadata) int {
	scoreA := fastModelScore(a)
	scoreB := fastModelScore(b)
	if scoreA != scoreB {
		return scoreB - scoreA
	}

	costA := combinedCost(a)
	costB := combinedCost(b)
	switch {
	case costA != costB:
		if costA < costB {
			return -1
		}
		return 1
	case a.ContextLimit != b.ContextLimit:
		if a.ContextLimit > 0 && b.ContextLimit > 0 {
			if a.ContextLimit < b.ContextLimit {
				return -1
			}
			return 1
		}
		if a.ContextLimit > 0 {
			return -1
		}
		if b.ContextLimit > 0 {
			return 1
		}
	case a.Created != b.Created:
		if a.Created > b.Created {
			return -1
		}
		return 1
	}
	return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
}

func fastModelScore(meta ModelMetadata) int {
	id := strings.ToLower(meta.ID)
	score := 0
	switch {
	case strings.Contains(id, "flash-lite"):
		score += 500
	case strings.Contains(id, "haiku"):
		score += 490
	case strings.Contains(id, "mini"):
		score += 480
	case strings.Contains(id, "lite"):
		score += 470
	case strings.Contains(id, "small"):
		score += 460
	case strings.Contains(id, "nano"):
		score += 450
	case strings.Contains(id, "flash"):
		score += 440
	case strings.Contains(id, "turbo"):
		score += 430
	}
	if meta.InputPriceKnown || meta.OutputPriceKnown {
		score += 50
	}
	if meta.ContextLimit > 0 && meta.ContextLimit <= 128000 {
		score += 10
	}
	return score
}

func combinedCost(meta ModelMetadata) float64 {
	cost := 0.0
	if meta.InputPriceKnown {
		cost += meta.InputPrice
	}
	if meta.OutputPriceKnown {
		cost += meta.OutputPrice
	}
	if cost == 0 && !meta.InputPriceKnown && !meta.OutputPriceKnown {
		return 1e12
	}
	return cost
}

func normalizeOptionalReasoning(value, fallback string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "":
		return fallback
	case "auto":
		return fallback
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
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
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	default:
		return fallback
	}
}

var fastModelTokens = []string{
	"flash-lite",
	"haiku",
	"mini",
	"lite",
	"small",
	"nano",
	"flash",
	"turbo",
}
