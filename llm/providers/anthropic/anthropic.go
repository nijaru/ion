package anthropic

import (
	"context"
	"os"

	sdk "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	"github.com/nijaru/ion/llm"
)

// Provider implements the llm.Provider interface for Anthropic.
type Provider struct {
	client sdk.Client
	config llm.ProviderConfig
	// modelCaps holds per-model capability overrides. Capabilities(model) looks
	// up this map before falling back to DefaultCapabilities.
	modelCaps map[string]llm.Capabilities
}

// New creates an Anthropic provider with the given API key.
// Use NewProvider for full configuration control.
func New(apiKey string) *Provider {
	return NewProvider(llm.ProviderConfig{ID: "anthropic", APIKey: apiKey})
}

// NewProvider creates a new Anthropic provider from a provider configuration.
func NewProvider(cfg llm.ProviderConfig) *Provider {
	apiKey := cfg.APIKey
	if apiKey == "" || apiKey == "$ANTHROPIC_API_KEY" {
		apiKey = os.Getenv("ANTHROPIC_API_KEY")
	}
	if cfg.ID == "" {
		cfg.ID = "anthropic"
	}

	opts := []option.RequestOption{
		option.WithAPIKey(apiKey),
		// Required for tool use in some versions of the API.
		option.WithHeader("anthropic-beta", "tools-2024-05-16"),
	}
	if cfg.APIEndpoint != "" {
		opts = append(opts, option.WithBaseURL(cfg.APIEndpoint))
	}

	return &Provider{
		client:    sdk.NewClient(opts...),
		config:    cfg,
		modelCaps: DefaultModelCaps(),
	}
}

func (p *Provider) ID() string {
	return string(p.config.ID)
}

func (p *Provider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	prepared, err := llm.PrepareRequestForCapabilities(req, p.Capabilities(req.Model))
	if err != nil {
		return nil, err
	}

	params := p.convertRequest(prepared)
	var opts []option.RequestOption
	if prepared.ThinkingBudget > 0 {
		opts = append(opts, option.WithHeader("anthropic-beta", "interleaved-thinking-2025-05-14"))
	}
	resp, err := p.client.Messages.New(ctx, params, opts...)
	if err != nil {
		return nil, err
	}

	usage := usageFromMessage(resp.Usage)
	usage.TotalTokens = usage.InputTokens + usage.OutputTokens
	usage.Cost = p.Cost(ctx, prepared.Model, usage)

	res := &llm.Response{
		Usage: usage,
	}

	for _, block := range resp.Content {
		switch block.Type {
		case "text":
			res.Content += block.Text
		case "thinking":
			res.Reasoning += block.Thinking
			res.ThinkingBlocks = append(res.ThinkingBlocks, llm.ThinkingBlock{
				Thinking:  block.Thinking,
				Signature: block.Signature,
			})
		case "redacted_thinking":
			res.Reasoning += "<redacted_thinking />"
			res.ThinkingBlocks = append(res.ThinkingBlocks, llm.ThinkingBlock{
				Redacted:  true,
				Signature: block.Signature,
			})
		case "tool_use":
			call := llm.Call{
				ID:   block.ID,
				Type: "function",
				Function: struct {
					Name      string `json:"name"`
					Arguments string `json:"arguments"`
				}{
					Name:      block.Name,
					Arguments: string(block.Input),
				},
			}
			res.Calls = append(res.Calls, call)

			// If this was a forced structured output, promote its input to Content.
			if rf := prepared.ResponseFormat; rf != nil && rf.Type == llm.ResponseFormatJSONSchema {
				name := rf.Name
				if name == "" {
					name = "json_response"
				}
				if block.Name == name {
					res.Content = string(block.Input)
				}
			}
			// "thinking" and "redacted_thinking" are internal reasoning blocks.
			// They are not exposed in Response to keep the API uniform.
		}
	}

	return res, nil
}

func (p *Provider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	prepared, err := llm.PrepareRequestForCapabilities(req, p.Capabilities(req.Model))
	if err != nil {
		return nil, err
	}

	params := p.convertRequest(prepared)
	var opts []option.RequestOption
	if prepared.ThinkingBudget > 0 {
		opts = append(opts, option.WithHeader("anthropic-beta", "interleaved-thinking-2025-05-14"))
	}
	stream := p.client.Messages.NewStreaming(ctx, params, opts...)

	targetName := ""
	if rf := prepared.ResponseFormat; rf != nil && rf.Type == llm.ResponseFormatJSONSchema {
		targetName = rf.Name
		if targetName == "" {
			targetName = "json_response"
		}
	}

	return &Stream{
		stream:     stream,
		targetName: targetName,
		model:      prepared.Model,
		p:          p,
		ctx:        ctx,
	}, nil
}

func (p *Provider) Models(ctx context.Context) ([]llm.Model, error) {
	return p.config.Models, nil
}

// CountTokens estimates tokens using per-message overhead heuristic.
// Accurate counting requires passing system + tools + messages to the
// Anthropic count_tokens API — deferred until Provider Capabilities are added.
func (p *Provider) CountTokens(_ context.Context, _ string, messages []llm.Message) (int, error) {
	total := 3 // reply priming
	for _, m := range messages {
		total += 4 // per-message overhead
		total += (len(m.TextContent()) + 3) / 4
		for _, call := range m.Calls {
			total += (len(call.Function.Name) + 3) / 4
			total += (len(call.Function.Arguments) + 3) / 4
		}
	}
	return total, nil
}

// Cost calculates the cost in USD based on the model configuration.
func (p *Provider) Cost(ctx context.Context, model string, usage llm.Usage) float64 {
	for _, m := range p.config.Models {
		if string(m.ID) == model {
			return (float64(usage.InputTokens) * m.CostPer1MIn / 1_000_000) + (float64(usage.OutputTokens) * m.CostPer1MOut / 1_000_000)
		}
	}
	return 0.0
}
