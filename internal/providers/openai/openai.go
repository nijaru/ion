package openai

import (
	"context"

	"github.com/nijaru/ion/internal/llm"
)

// Provider implements the llm.Provider interface for OpenAI.
type Provider struct {
	Base
}

// New creates an OpenAI provider with the given API key.
// Use NewProvider for full configuration control.
func New(apiKey string) *Provider {
	return NewProvider(llm.ProviderConfig{ID: "openai", APIKey: apiKey})
}

// NewProvider creates a new OpenAI provider from a provider configuration.
func NewProvider(cfg llm.ProviderConfig) *Provider {
	return NewCompatibleProvider(cfg, CompatibleSpec{
		ID:                 "openai",
		DefaultAPIEndpoint: "https://api.openai.com/v1",
		APIKeyEnvVars:      []string{"OPENAI_API_KEY"},
		ModelCaps:          DefaultModelCaps(),
	})
}

func (p *Provider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	return p.Base.Generate(ctx, req)
}

func (p *Provider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	return p.Base.Stream(ctx, req)
}
