package ollama

import (
	"context"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/providers/openai"
)

// Provider implements the llm.Provider interface for Ollama.
// It uses Ollama's OpenAI-compatible API endpoint.
type Provider struct {
	openai.Base
}

// New creates an Ollama provider pointing at the default local endpoint.
// Use NewProvider for custom base URL or configuration.
func New() *Provider {
	return NewProvider(llm.ProviderConfig{ID: "ollama", APIEndpoint: "http://localhost:11434/v1"})
}

// NewProvider creates a new Ollama provider from a provider configuration.
func NewProvider(cfg llm.ProviderConfig) *Provider {
	p := openai.NewCompatibleProvider(cfg, openai.CompatibleSpec{
		ID:                 "ollama",
		DefaultAPIEndpoint: "http://localhost:11434/v1",
		APIKeyEnvVars:      []string{"OLLAMA_API_KEY"},
		DefaultAPIKey:      "ollama",
	})
	return &Provider{Base: p.Base}
}

func (p *Provider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	return p.Base.Generate(ctx, req)
}

func (p *Provider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	return p.Base.Stream(ctx, req)
}
