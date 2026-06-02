package providers

import (
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
func NewProviderFromConfig(cfg *config.Config) (llm.Provider, error) {
	if cfg == nil {
		return nil, fmt.Errorf("config is nil")
	}

	providerName := strings.TrimSpace(cfg.Provider)
	if providerName == "" {
		return nil, fmt.Errorf("provider not specified")
	}

	def, ok := llm.Lookup(providerName)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", providerName)
	}

	apiKey := llm.ResolvedAuthToken(cfg, def)
	endpoint := llm.ResolvedEndpoint(cfg)

	providerCfg := llm.ProviderConfig{
		ID:             def.ID,
		APIKey:         apiKey,
		APIEndpoint:    endpoint,
		DefaultHeaders: llm.ResolvedHeaders(cfg),
		Models:         configModels(cfg),
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
