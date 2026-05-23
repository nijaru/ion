package canto

import (
	"context"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/ion/internal/config"
)

func syncCantoSessionSettings(
	ctx context.Context,
	session *cantofw.Session,
	cfg *config.Config,
) error {
	if session == nil {
		return nil
	}
	model := modelFromConfig(cfg)
	provider := providerFromConfig(cfg)
	settings, err := session.EffectiveSettings(ctx)
	if err != nil {
		return err
	}
	if model != "" &&
		(!settings.HasModel ||
			settings.Model.Model != model ||
			(provider != "" && settings.Model.ProviderID != provider)) {
		if err := session.SetModel(ctx, model); err != nil {
			return err
		}
	}

	level := config.DefaultReasoningEffort
	if cfg != nil {
		level = normalizeReasoningEffort(cfg.ReasoningEffort)
	}
	if settings.ThinkingLevel != level {
		if err := session.SetThinkingLevel(ctx, level); err != nil {
			return err
		}
	}
	return nil
}
