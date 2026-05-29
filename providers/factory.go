package providers

import (
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/providers/anthropic"
	"github.com/nijaru/ion/providers/gemini"
	"github.com/nijaru/ion/providers/ollama"
	"github.com/nijaru/ion/providers/openai"
	"github.com/nijaru/ion/providers/openrouter"
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

	def, ok := Lookup(providerName)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", providerName)
	}

	apiKey := ResolvedAuthToken(cfg, def)
	endpoint := ResolvedEndpoint(cfg)

	providerCfg := llm.ProviderConfig{
		ID:             def.ID,
		APIKey:         apiKey,
		APIEndpoint:    endpoint,
		DefaultHeaders: ResolvedHeaders(cfg),
		Models:         configModels(cfg),
	}

	switch def.Family {
	case FamilyAnthropic:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
		}
		return anthropic.NewProvider(providerCfg), nil

	case FamilyOpenAI:
		if RequiresAuth(cfg, def) && apiKey == "" {
			return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
		}
		return openai.NewProvider(providerCfg), nil

	case FamilyOpenRouter:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
		}
		return openrouter.NewProvider(providerCfg), nil

	case FamilyGemini:
		if apiKey == "" {
			return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
		}
		return gemini.NewProvider(providerCfg), nil

	case FamilyOllama:
		return ollama.NewProvider(providerCfg), nil

	default:
		return nil, fmt.Errorf("unsupported provider family %q", def.Family)
	}
}

// configModels creates basic model definitions from config without metadata.
// The caller should enrich these with models.GetCachedMetadata if available.
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
