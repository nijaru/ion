package providers

import (
	"fmt"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers/anthropic"
	"github.com/nijaru/ion/llm/providers/gemini"
	"github.com/nijaru/ion/llm/providers/ollama"
	openaipkg "github.com/nijaru/ion/llm/providers/openai"
	"github.com/nijaru/ion/llm/providers/openrouter"
)

type Config struct {
	APIKey   string
	Endpoint string
	Headers  map[string]string
	Models   []llm.Model
}

type OpenAICompatibleConfig struct {
	ID            string
	APIKey        string
	Endpoint      string
	APIKeyEnvVars []string
	Headers       map[string]string
	Models        []llm.Model
	ModelCaps     map[string]llm.Capabilities
	DefaultAPIKey string
}

func Anthropic() llm.Provider {
	return NewAnthropic(Config{})
}

func NewAnthropic(config Config) llm.Provider {
	return anthropic.NewProvider(buildConfig("anthropic", config))
}

func OpenAI() llm.Provider {
	return NewOpenAI(Config{})
}

func NewOpenAI(config Config) llm.Provider {
	return openaipkg.NewProvider(buildConfig("openai", config))
}

func OpenRouter() llm.Provider {
	return NewOpenRouter(Config{})
}

func NewOpenRouter(config Config) llm.Provider {
	return openrouter.NewProvider(buildConfig("openrouter", config))
}

func Gemini() llm.Provider {
	return NewGemini(Config{})
}

func NewGemini(config Config) llm.Provider {
	return gemini.NewProvider(buildConfig("gemini", config))
}

func Ollama() llm.Provider {
	return NewOllama(Config{})
}

func NewOllama(config Config) llm.Provider {
	return ollama.NewProvider(buildConfig("ollama", config))
}

func NewOpenAICompatible(config OpenAICompatibleConfig) (llm.Provider, error) {
	if config.ID == "" {
		return nil, fmt.Errorf("provider id is required")
	}

	cfg := llm.ProviderConfig{
		ID:             config.ID,
		APIKey:         config.APIKey,
		APIEndpoint:    config.Endpoint,
		DefaultHeaders: cloneHeaders(config.Headers),
		Models:         append([]llm.Model(nil), config.Models...),
	}

	return openaipkg.NewCompatibleProvider(cfg, openaipkg.CompatibleSpec{
		ID:                 config.ID,
		DefaultAPIEndpoint: config.Endpoint,
		APIKeyEnvVars:      append([]string(nil), config.APIKeyEnvVars...),
		DefaultHeaders:     cloneHeaders(config.Headers),
		ModelCaps:          config.ModelCaps,
		DefaultAPIKey:      config.DefaultAPIKey,
	}), nil
}

func buildConfig(id string, config Config) llm.ProviderConfig {
	return llm.ProviderConfig{
		ID:             id,
		APIKey:         config.APIKey,
		APIEndpoint:    config.Endpoint,
		DefaultHeaders: cloneHeaders(config.Headers),
		Models:         append([]llm.Model(nil), config.Models...),
	}
}

func cloneHeaders(src map[string]string) map[string]string {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]string, len(src))
	for k, v := range src {
		dst[k] = v
	}
	return dst
}
