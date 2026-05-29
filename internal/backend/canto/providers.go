package canto

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/canto/llm"
	cproviders "github.com/nijaru/canto/llm/providers"
	"github.com/nijaru/ion/internal/models"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/privacy"
	"github.com/nijaru/ion/internal/providers"
)

var providerFactory = newProvider

func configureRetryProvider(p llm.Provider, cfg *config.Config) llm.Provider {
	retry, ok := p.(*llm.RetryProvider)
	if !ok {
		retry = llm.NewRetryProvider(p)
	}
	retry.Config.RetryForever = cfg.RetryUntilCancelledEnabled()
	retry.Config.RetryForeverTransportOnly = true
	retry.Config.OnRetry = nil
	return retry
}

type providerRetryOwner struct {
	llm.Provider
}

func (p providerRetryOwner) IsTransient(error) bool {
	return false
}

// useProviderRetryOnly makes provider retry/backoff the single native retry owner.
func useProviderRetryOnly(p llm.Provider) llm.Provider {
	if p == nil {
		return nil
	}
	return providerRetryOwner{Provider: p}
}

func retryProviderInChain(p llm.Provider) (*llm.RetryProvider, bool) {
	switch provider := p.(type) {
	case nil:
		return nil, false
	case *llm.RetryProvider:
		return provider, true
	case providerRetryOwner:
		return retryProviderInChain(provider.Provider)
	case requestObservingProvider:
		return retryProviderInChain(provider.Provider)
	default:
		return nil, false
	}
}

func retryStatus(event llm.RetryEvent) string {
	delay := event.Delay.Round(time.Second)
	if delay <= 0 {
		delay = event.Delay
	}
	label := "Provider error"
	if llm.IsTransientTransportError(event.Err) {
		label = "Network error"
	}
	if detail := retryErrorDetail(event.Err); detail != "" {
		label += ": " + detail
	}
	if delay > 0 {
		return fmt.Sprintf(
			"%s. Retrying in %s... Ctrl+C stops.",
			label,
			delay,
		)
	}
	return label + ". Retrying... Ctrl+C stops."
}

func newProvider(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
	if cfg == nil {
		return nil, fmt.Errorf("provider config not set")
	}

	// Synchronize Framework Capabilities Registry with Ion overrides at startup
	llm.ClearRegistry()
	for _, mDef := range cfg.Models {
		preset := llm.ModelPreset(mDef.Preset)
		var customCaps *llm.Capabilities
		if mDef.Temperature != nil || mDef.SystemRole != "" || mDef.ReasoningKind != "" {
			var caps llm.Capabilities
			if preset == llm.PresetReasoning {
				caps = llm.Capabilities{
					Streaming:   true,
					Tools:       true,
					Temperature: false,
					SystemRole:  llm.RoleSystem,
					Reasoning: llm.ReasoningCapabilities{
						Kind:       llm.ReasoningKindEffort,
						Efforts:    []string{"minimal", "low", "medium", "high"},
						CanDisable: true,
					},
				}
			} else if preset == llm.PresetOpenAIReasoning {
				caps = llm.Capabilities{
					Streaming:   true,
					Tools:       true,
					Temperature: false,
					SystemRole:  llm.RoleDeveloper,
					Reasoning: llm.ReasoningCapabilities{
						Kind:       llm.ReasoningKindEffort,
						Efforts:    []string{"minimal", "low", "medium", "high"},
						CanDisable: true,
					},
				}
			} else {
				caps = llm.DefaultCapabilities()
			}

			if mDef.Temperature != nil {
				caps.Temperature = *mDef.Temperature
			}
			if mDef.SystemRole != "" {
				switch strings.ToLower(strings.TrimSpace(mDef.SystemRole)) {
				case "system":
					caps.SystemRole = llm.RoleSystem
				case "user":
					caps.SystemRole = llm.RoleUser
				case "developer":
					caps.SystemRole = llm.RoleDeveloper
				}
			}
			if mDef.ReasoningKind != "" {
				switch strings.ToLower(strings.TrimSpace(mDef.ReasoningKind)) {
				case "effort":
					caps.Reasoning.Kind = llm.ReasoningKindEffort
					caps.Reasoning.Efforts = []string{"minimal", "low", "medium", "high"}
					caps.Reasoning.CanDisable = true
				case "budget":
					caps.Reasoning.Kind = llm.ReasoningKindBudget
					caps.Reasoning.BudgetMinTokens = 1024
				case "boolean":
					caps.Reasoning.Kind = llm.ReasoningKindBoolean
					caps.Reasoning.CanDisable = true
				case "none":
					caps.Reasoning.Kind = llm.ReasoningKindNone
				}
			}
			customCaps = &caps
		}
		llm.RegisterModel(llm.ModelDef{
			Pattern:      mDef.Pattern,
			Preset:       preset,
			Capabilities: customCaps,
		})
	}

	def, ok := providers.Lookup(cfg.Provider)
	if !ok {
		return nil, fmt.Errorf("unsupported canto provider %q", cfg.Provider)
	}
	apiKey := providers.ResolvedAuthToken(cfg, def)
	endpoint := providers.ResolvedEndpointContext(ctx, cfg)
	models := providerModels(cfg)
	switch def.Family {
	case providers.FamilyAnthropic:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", providers.MissingAuthDetail(cfg, def))
		}
		return cproviders.NewAnthropic(cproviders.Config{
			APIKey:   apiKey,
			Endpoint: endpoint,
			Headers:  providers.ResolvedHeaders(cfg),
			Models:   models,
		}), nil
	case providers.FamilyOpenAI:
		if providers.RequiresAuth(cfg, def) && apiKey == "" {
			return nil, fmt.Errorf("%s not set", providers.MissingAuthDetail(cfg, def))
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
			ModelCaps: nil, // falls back cleanly to Canto registry
		})
	case providers.FamilyOpenRouter:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", providers.MissingAuthDetail(cfg, def))
		}
		return cproviders.NewOpenRouter(cproviders.Config{
			APIKey:   apiKey,
			Endpoint: endpoint,
			Headers:  providers.ResolvedHeaders(cfg),
			Models:   models,
		}), nil
	case providers.FamilyGemini:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", providers.MissingAuthDetail(cfg, def))
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

func providerModels(cfg *config.Config) []llm.Model {
	if cfg == nil || strings.TrimSpace(cfg.Provider) == "" || strings.TrimSpace(cfg.Model) == "" {
		return nil
	}
	model := llm.Model{ID: cfg.Model}
	var isReasoning bool
	if meta, ok := models.GetCachedMetadata(cfg.Provider, cfg.Model); ok {
		model.ContextWindow = meta.ContextLimit
		model.CostPer1MIn = meta.InputPrice
		model.CostPer1MOut = meta.OutputPrice
		isReasoning = meta.Reasoning
	}
	if cfg.ContextLimit > 0 {
		model.ContextWindow = cfg.ContextLimit
	}
	if isReasoning {
		caps := llm.Capabilities{
			Streaming:   true,
			Tools:       true,
			Temperature: false,
			SystemRole:  llm.RoleSystem,
			Reasoning: llm.ReasoningCapabilities{
				Kind:       llm.ReasoningKindEffort,
				Efforts:    []string{"minimal", "low", "medium", "high"},
				CanDisable: true,
			},
		}
		model.Capabilities = &caps
	}
	return []llm.Model{model}
}

func missingAuthDetail(cfg *config.Config, def providers.Definition) string {
	return providers.MissingAuthDetail(cfg, def)
}

func retryErrorDetail(err error) string {
	if err == nil {
		return ""
	}
	detail := strings.Join(strings.Fields(privacy.Redact(err.Error())), " ")
	const maxLen = 160
	if len(detail) <= maxLen {
		return detail
	}
	return strings.TrimSpace(detail[:maxLen-3]) + "..."
}
