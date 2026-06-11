package openai

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/llm"
	"github.com/sashabaranov/go-openai"
)

// Base implements the core OpenAI-compatible provider logic.
// Providers like Ollama, OpenRouter, and OpenAI itself can embed or wrap this.
type Base struct {
	Client *openai.Client
	Config llm.ProviderConfig
	// ModelCaps holds per-model capability overrides. Capabilities(model) looks
	// up this map before falling back to DefaultCapabilities. Populate with
	// DefaultModelCaps() to get known reasoning model entries.
	ModelCaps map[string]llm.Capabilities
	// Compat holds provider-specific compatibility settings.
	// If nil, defaults are auto-detected from the provider ID and endpoint.
	Compat *llm.ProviderCompat
}

// CompatSettings returns the provider compatibility settings.
// If explicitly set, returns those. Otherwise auto-detects from provider ID and endpoint.
func (b *Base) CompatSettings() llm.ProviderCompat {
	if b.Compat != nil {
		return *b.Compat
	}
	return llm.DetectCompat(string(b.Config.ID), b.Config.APIEndpoint)
}

// ID returns the unique identifier for this provider.
func (b *Base) ID() string {
	return string(b.Config.ID)
}

// Models returns the list of models supported by this provider.
func (b *Base) Models(ctx context.Context) ([]llm.Model, error) {
	return b.Config.Models, nil
}

// CountTokens estimates tokens using per-message overhead documented by OpenAI:
// 3 tokens for reply priming, 4 tokens per message for role/delimiter encoding.
// Content is estimated at 1 token per 4 chars.
func (b *Base) CountTokens(_ context.Context, _ string, messages []llm.Message) (int, error) {
	total := 3 // reply priming
	for _, m := range messages {
		total += 4 // per-message overhead
		total += (len(m.TextContent()) + 3) / 4
		for _, call := range m.BlocksToolCalls() {
			total += (len(call.Function.Name) + 3) / 4
			total += (len(call.Function.Arguments) + 3) / 4
		}
	}
	return total, nil
}

// Cost calculates the cost in USD based on the model configuration.
func (b *Base) Cost(ctx context.Context, model string, usage llm.Usage) float64 {
	for _, m := range b.Config.Models {
		if string(m.ID) == model {
			return (float64(usage.InputTokens) * m.CostPer1MIn / 1_000_000) + (float64(usage.OutputTokens) * m.CostPer1MOut / 1_000_000)
		}
	}
	return 0.0
}

// Generate handles the OpenAI-compatible chat completion.
func (b *Base) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	prepared, err := llm.PrepareRequestForCapabilities(req, b.Capabilities(req.Model))
	if err != nil {
		return nil, err
	}

	resp, err := b.Client.CreateChatCompletion(ctx, b.ConvertRequest(prepared))
	if err != nil {
		return nil, err
	}

	if len(resp.Choices) == 0 {
		return nil, fmt.Errorf("no choices returned from %s", b.Config.ID)
	}

	choice := resp.Choices[0]
	usage := llm.Usage{
		InputTokens:  resp.Usage.PromptTokens,
		OutputTokens: resp.Usage.CompletionTokens,
		TotalTokens:  resp.Usage.TotalTokens,
	}
	usage.Cost = b.Cost(ctx, prepared.Model, usage)

	return &llm.Response{
		Usage:  usage,
		Blocks: buildBlocks(choice.Message.Content, choice.Message.ReasoningContent, choice.Message.ToolCalls),
	}, nil
}

// Stream handles the OpenAI-compatible streaming chat completion.
func (b *Base) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	prepared, err := llm.PrepareRequestForCapabilities(req, b.Capabilities(req.Model))
	if err != nil {
		return nil, err
	}

	stream, err := b.Client.CreateChatCompletionStream(ctx, b.ConvertRequest(prepared))
	if err != nil {
		return nil, err
	}

	return &OpenAIStream{
		stream:      stream,
		activeCalls: make(map[int]llm.Call),
	}, nil
}
