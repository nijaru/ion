package providers

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers/anthropic"
	"github.com/nijaru/ion/llm/providers/gemini"
	"github.com/nijaru/ion/llm/providers/ollama"
	"github.com/nijaru/ion/llm/providers/openai"
	"github.com/nijaru/ion/llm/providers/openrouter"
)

// NewProviderFromConfig creates an llm.Provider from a config.Config.
// Model metadata (context limits, pricing) is resolved separately by the caller
// to avoid a circular dependency between providers and models.
func NewProviderFromConfig(
	ctx context.Context,
	cfg *config.Config,
	resolver *llm.EndpointResolver,
) (llm.Provider, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if cfg == nil {
		return nil, fmt.Errorf("config is nil")
	}
	if resolver == nil {
		return nil, fmt.Errorf("endpoint resolver is required")
	}

	providerName := strings.TrimSpace(cfg.Provider)
	if providerName == "" {
		return nil, fmt.Errorf("provider not specified")
	}

	def, ok := llm.LookupConfig(cfg, providerName)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", providerName)
	}

	apiKey := llm.ResolvedAuthToken(cfg, def)
	endpoint := resolver.Resolve(ctx, cfg)
	modelRegistry, err := modelRegistryFromConfig(cfg)
	if err != nil {
		return nil, err
	}

	providerCfg := llm.ProviderConfig{
		ID:             def.ID,
		APIKey:         apiKey,
		APIEndpoint:    endpoint,
		DefaultHeaders: llm.ResolvedHeaders(cfg),
		Models:         configModels(cfg),
		ModelRegistry:  modelRegistry,
		ModelRouting:   convertRouting(providerName, cfg.Providers),
	}

	switch def.Family {
	case llm.FamilyAnthropic:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", llm.MissingAuthDetail(cfg, def))
		}
		return anthropic.NewProvider(providerCfg), nil

	case llm.FamilyOpenAI:
		if llm.RequiresAuth(cfg, def) && apiKey == "" {
			return nil, fmt.Errorf("%s not set", llm.MissingAuthDetail(cfg, def))
		}
		return openai.NewProvider(providerCfg), nil

	case llm.FamilyOpenRouter:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", llm.MissingAuthDetail(cfg, def))
		}
		return openrouter.NewProvider(providerCfg), nil

	case llm.FamilyGemini:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", llm.MissingAuthDetail(cfg, def))
		}
		return gemini.NewProvider(providerCfg), nil

	case llm.FamilyOllama:
		return ollama.NewProvider(providerCfg), nil

	default:
		return nil, fmt.Errorf("unsupported provider family %q", def.Family)
	}
}

func modelRegistryFromConfig(cfg *config.Config) (*llm.Registry, error) {
	if cfg == nil || len(cfg.Models) == 0 {
		return nil, nil
	}

	registry := llm.NewRegistry()
	for _, def := range cfg.Models {
		pattern := strings.TrimSpace(def.Pattern)
		if pattern == "" {
			return nil, fmt.Errorf("model capability pattern is empty")
		}

		role := llm.Role(strings.ToLower(strings.TrimSpace(def.SystemRole)))
		switch role {
		case "", llm.RoleSystem, llm.RoleUser, llm.RoleDeveloper:
		default:
			return nil, fmt.Errorf("model %q has invalid system role %q", pattern, def.SystemRole)
		}

		preset := llm.ModelPreset(strings.ToLower(strings.TrimSpace(def.Preset)))
		switch preset {
		case "", llm.PresetChat, llm.PresetReasoning, llm.PresetOpenAIReasoning:
		default:
			return nil, fmt.Errorf("model %q has invalid capability preset %q", pattern, def.Preset)
		}

		reasoningKind := llm.ReasoningKind(strings.ToLower(strings.TrimSpace(def.ReasoningKind)))
		switch reasoningKind {
		case "", llm.ReasoningKindEffort, llm.ReasoningKindBudget, llm.ReasoningKindBoolean:
		default:
			return nil, fmt.Errorf("model %q has invalid reasoning kind %q", pattern, def.ReasoningKind)
		}

		registry.Register(llm.ModelDef{
			Pattern:       pattern,
			Preset:        preset,
			Temperature:   def.Temperature,
			SystemRole:    role,
			ReasoningKind: reasoningKind,
		})
	}
	return registry, nil
}

// configModels creates basic model definitions from config without metadata.
// The caller should enrich these with llm.GetCachedMetadata if available.
func configModels(cfg *config.Config) []llm.Model {
	if cfg == nil || strings.TrimSpace(cfg.Provider) == "" || strings.TrimSpace(cfg.Model) == "" {
		return nil
	}

	model := llm.Model{ID: cfg.Model}
	if cfg.ContextLimit > 0 {
		model.ContextWindow = cfg.ContextLimit
	}

	return []llm.Model{model}
}

// convertRouting extracts model routing for a provider from config.
func convertRouting(providerName string, providers map[string]config.ProviderSettings) map[string]llm.ModelRouting {
	ps, ok := providers[providerName]
	if !ok || len(ps.ModelRouting) == 0 {
		return nil
	}
	out := make(map[string]llm.ModelRouting, len(ps.ModelRouting))
	for id, r := range ps.ModelRouting {
		out[id] = llm.ModelRouting{
			Order:          r.Order,
			Only:           r.Only,
			AllowFallbacks: r.AllowFallbacks,
		}
	}
	return out
}
