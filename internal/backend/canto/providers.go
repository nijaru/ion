package canto

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/nijaru/canto/llm"
	cproviders "github.com/nijaru/canto/llm/providers"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	ionsession "github.com/nijaru/ion/internal/session"
)

var providerFactory = newProvider

func configureRetryProvider(
	p llm.Provider,
	cfg *config.Config,
	events chan<- ionsession.Event,
) llm.Provider {
	if retry, ok := p.(*llm.RetryProvider); ok {
		return retry
	}
	retry := llm.NewRetryProvider(p)
	retry.Config.RetryForever = cfg.RetryUntilCancelledEnabled()
	retry.Config.RetryForeverTransportOnly = true
	retry.Config.OnRetry = func(event llm.RetryEvent) {
		events <- ionsession.StatusChanged{Status: retryStatus(event)}
	}
	return retry
}

func retryStatus(event llm.RetryEvent) string {
	delay := event.Delay.Round(time.Second)
	if delay <= 0 {
		delay = event.Delay
	}
	kind := "Provider error"
	if llm.IsTransientTransportError(event.Err) {
		kind = "Network error"
	}
	if delay > 0 {
		return fmt.Sprintf(
			"%s. Retrying in %s... Ctrl+C stops.",
			kind,
			delay,
		)
	}
	return kind + ". Retrying... Ctrl+C stops."
}

func newProvider(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
	if cfg == nil {
		return nil, fmt.Errorf("provider config not set")
	}
	def, ok := providers.Lookup(cfg.Provider)
	if !ok {
		return nil, fmt.Errorf("unsupported canto provider %q", cfg.Provider)
	}
	models := providerModels(ctx, cfg)
	apiKey := resolvedAPIKey(cfg, def)
	endpoint := providers.ResolvedEndpointContext(ctx, cfg)
	switch def.Family {
	case providers.FamilyAnthropic:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", missingAuthDetail(cfg, def))
		}
		return cproviders.NewAnthropic(cproviders.Config{
			APIKey:   apiKey,
			Endpoint: endpoint,
			Headers:  providers.ResolvedHeaders(cfg),
			Models:   models,
		}), nil
	case providers.FamilyOpenAI:
		if def.AuthKind != providers.AuthLocal && apiKey == "" {
			return nil, fmt.Errorf("%s not set", missingAuthDetail(cfg, def))
		}
		if def.ID == "openai" {
			return cproviders.NewOpenAI(cproviders.Config{
				APIKey:   apiKey,
				Endpoint: endpoint,
				Headers:  providers.ResolvedHeaders(cfg),
				Models:   models,
			}), nil
		}
		return cproviders.NewOpenAICompatible(cproviders.OpenAICompatibleConfig{
			ID:        def.ID,
			APIKey:    apiKey,
			Endpoint:  endpoint,
			Headers:   providers.ResolvedHeaders(cfg),
			Models:    models,
			ModelCaps: nil,
		})
	case providers.FamilyOpenRouter:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", missingAuthDetail(cfg, def))
		}
		return cproviders.NewOpenRouter(cproviders.Config{
			APIKey:   apiKey,
			Endpoint: endpoint,
			Headers:  providers.ResolvedHeaders(cfg),
			Models:   models,
		}), nil
	case providers.FamilyGemini:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", missingAuthDetail(cfg, def))
		}
		return cproviders.NewGemini(cproviders.Config{
			APIKey:   apiKey,
			Endpoint: endpoint,
			Headers:  providers.ResolvedHeaders(cfg),
			Models:   models,
		}), nil
	case providers.FamilyOllama:
		return cproviders.NewOllama(cproviders.Config{
			APIKey:   apiKey,
			Endpoint: endpoint,
			Headers:  providers.ResolvedHeaders(cfg),
			Models:   models,
		}), nil
	default:
		return nil, fmt.Errorf("unsupported provider family %q", def.Family)
	}
}

func providerModels(ctx context.Context, cfg *config.Config) []llm.Model {
	if cfg == nil || strings.TrimSpace(cfg.Provider) == "" || strings.TrimSpace(cfg.Model) == "" {
		return nil
	}
	meta, ok := registry.GetMetadata(ctx, cfg.Provider, cfg.Model)
	if !ok {
		return []llm.Model{{ID: cfg.Model}}
	}
	return []llm.Model{
		{
			ID:            cfg.Model,
			ContextWindow: meta.ContextLimit,
			CostPer1MIn:   meta.InputPrice,
			CostPer1MOut:  meta.OutputPrice,
		},
	}
}

func resolvedAPIKey(cfg *config.Config, def providers.Definition) string {
	if def.AuthKind == providers.AuthLocal {
		return ""
	}
	names := []string{}
	if override := strings.TrimSpace(cfg.AuthEnvVar); override != "" {
		names = append(names, override)
	}
	if def.DefaultEnvVar != "" {
		names = append(names, def.DefaultEnvVar)
	}
	names = append(names, def.AlternateEnvVars...)
	for _, name := range names {
		if value := strings.TrimSpace(os.Getenv(name)); value != "" {
			return value
		}
	}
	return ""
}

func missingAuthDetail(cfg *config.Config, def providers.Definition) string {
	if override := strings.TrimSpace(cfg.AuthEnvVar); override != "" {
		return override
	}
	if def.DefaultEnvVar != "" {
		return def.DefaultEnvVar
	}
	return "provider credentials"
}
