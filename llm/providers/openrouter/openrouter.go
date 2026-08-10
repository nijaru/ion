package openrouter

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers/openai"
	sashaoai "github.com/sashabaranov/go-openai"
)

// openRouterProviderRouting configures OpenRouter's provider routing for a model.
type openRouterProviderRouting struct {
	Order          []string `json:"order,omitempty"`
	Only           []string `json:"only,omitempty"`
	AllowFallbacks bool     `json:"allow_fallbacks"`
}

// Provider implements the llm.Provider interface for OpenRouter.
// It overrides Generate/Stream to use OpenRouter's nested reasoning format:
//
//	{ "reasoning": { "effort": "high" } }
//
// instead of OpenAI's top-level reasoning_effort string, which OpenRouter
// rejects with 422 for models that require the nested format.
type Provider struct {
	openai.Base
	httpClient   *http.Client
	endpoint     string
	apiKey       string
	headers      map[string]string
	modelRouting map[string]openRouterProviderRouting // model ID → routing config
}

// NewProvider creates a new OpenRouter provider from a provider configuration.
func NewProvider(cfg llm.ProviderConfig) *Provider {
	p := openai.NewCompatibleProvider(cfg, openai.CompatibleSpec{
		ID:                 "openrouter",
		DefaultAPIEndpoint: "https://openrouter.ai/api/v1",
		APIKeyEnvVars:      []string{"OPENROUTER_API_KEY"},
	})

	endpoint := cfg.APIEndpoint
	if endpoint == "" {
		endpoint = "https://openrouter.ai/api/v1"
	}

	apiKey := cfg.APIKey
	if apiKey == "" {
		apiKey = os.Getenv("OPENROUTER_API_KEY")
	}

	modelRouting := make(map[string]openRouterProviderRouting, len(cfg.ModelRouting))
	for id, r := range cfg.ModelRouting {
		modelRouting[id] = openRouterProviderRouting{
			Order:          r.Order,
			Only:           r.Only,
			AllowFallbacks: r.AllowFallbacks,
		}
	}

	return &Provider{
		Base:         p.Base,
		httpClient:   &http.Client{},
		endpoint:     strings.TrimRight(endpoint, "/"),
		apiKey:       apiKey,
		headers:      cfg.DefaultHeaders,
		modelRouting: modelRouting,
	}
}

func (p *Provider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	caps := llm.CapabilitiesForRequest(req, p.Base.Capabilities(req.Model))
	prepared, err := llm.PrepareRequestForCapabilities(req, caps)
	if err != nil {
		return nil, err
	}

	body, err := p.buildAndMarshalRequest(prepared)
	if err != nil {
		return nil, fmt.Errorf("openrouter: build request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(
		ctx,
		http.MethodPost,
		p.endpoint+"/chat/completions",
		bytes.NewReader(body),
	)
	if err != nil {
		return nil, fmt.Errorf("openrouter: create request: %w", err)
	}
	p.setHeaders(httpReq)
	for k, v := range req.Headers {
		httpReq.Header.Set(k, v)
	}

	httpResp, err := p.client(prepared).Do(httpReq)
	if err != nil {
		return nil, err
	}
	defer httpResp.Body.Close()

	respBody, err := io.ReadAll(httpResp.Body)
	if err != nil {
		return nil, fmt.Errorf("openrouter: read response: %w", err)
	}

	if httpResp.StatusCode != http.StatusOK {
		retryAfter, _ := llm.ParseRetryAfter(httpResp.Header.Get("Retry-After"), time.Now())
		return nil, llm.NewHTTPErrorWithRetryAfter("openrouter", httpResp.StatusCode, respBody, retryAfter)
	}

	var resp sashaoai.ChatCompletionResponse
	if err := json.Unmarshal(respBody, &resp); err != nil {
		return nil, fmt.Errorf("openrouter: decode response: %w", err)
	}

	if len(resp.Choices) == 0 {
		return nil, fmt.Errorf("openrouter: no choices returned")
	}

	choice := resp.Choices[0]
	usage := llm.Usage{
		InputTokens:  resp.Usage.PromptTokens,
		OutputTokens: resp.Usage.CompletionTokens,
		TotalTokens:  resp.Usage.TotalTokens,
	}
	usage.Cost = p.Base.Cost(ctx, prepared.Model, usage)

	return &llm.Response{
		Blocks:        p.buildBlocks(choice.Message.Content, choice.Message.ReasoningContent, choice.Message.ToolCalls),
		Usage:         usage,
		Model:         prepared.Model,
		ResponseModel: responseModel(resp.Model, prepared.Model),
		ResponseID:    resp.ID,
		StopReason:    mapFinishReason(string(choice.FinishReason)),
	}, nil
}

func (p *Provider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	caps := llm.CapabilitiesForRequest(req, p.Base.Capabilities(req.Model))
	prepared, err := llm.PrepareRequestForCapabilities(req, caps)
	if err != nil {
		return nil, err
	}

	body, err := p.buildAndMarshalRequestStream(prepared)
	if err != nil {
		return nil, fmt.Errorf("openrouter: build request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(
		ctx,
		http.MethodPost,
		p.endpoint+"/chat/completions",
		bytes.NewReader(body),
	)
	if err != nil {
		return nil, fmt.Errorf("openrouter: create request: %w", err)
	}
	p.setHeaders(httpReq)
	for k, v := range req.Headers {
		httpReq.Header.Set(k, v)
	}

	httpResp, err := p.client(prepared).Do(httpReq)
	if err != nil {
		return nil, err
	}

	if httpResp.StatusCode != http.StatusOK {
		defer httpResp.Body.Close()
		respBody, _ := io.ReadAll(httpResp.Body)
		retryAfter, _ := llm.ParseRetryAfter(httpResp.Header.Get("Retry-After"), time.Now())
		return nil, llm.NewHTTPErrorWithRetryAfter("openrouter", httpResp.StatusCode, respBody, retryAfter)
	}

	return &openRouterStream{
		body:     httpResp.Body,
		reader:   httpResp.Body,
		provider: p,
		ctx:      ctx,
		model:    prepared.Model,
	}, nil
}

func responseModel(actual, requested string) string {
	if actual != "" && actual != requested {
		return actual
	}
	return ""
}

func mapFinishReason(reason string) llm.StopReason {
	switch reason {
	case "stop", "eos_token", "stop_sequence":
		return llm.StopReasonStop
	case "length", "max_tokens":
		return llm.StopReasonLength
	case "tool_calls", "function_call":
		return llm.StopReasonToolUse
	case "content_filter", "error":
		return llm.StopReasonError
	default:
		return ""
	}
}

func (p *Provider) client(req *llm.Request) *http.Client {
	if req.Transport != nil {
		return &http.Client{Transport: req.Transport}
	}
	return p.httpClient
}

func (p *Provider) setHeaders(req *http.Request) {
	req.Header.Set("Content-Type", "application/json")
	if p.apiKey != "" {
		req.Header.Set("Authorization", "Bearer "+p.apiKey)
	}
	req.Header.Set("User-Agent", "ion/0.0.0")
	// OpenRouter attribution headers for analytics
	req.Header.Set("HTTP-Referer", "https://ion.dev")
	req.Header.Set("X-Title", "ion")
	for k, v := range p.headers {
		req.Header.Set(k, v)
	}
}

// openRouterRequest wraps the standard OpenAI request to add OpenRouter-specific
// fields. The Reasoning field uses OpenRouter's nested format.
type openRouterRequest struct {
	sashaoai.ChatCompletionRequest
	Reasoning *openRouterReasoning `json:"reasoning,omitempty"`
	// SessionID enables OpenRouter's sticky routing for consistent provider selection.
	// See: https://openrouter.ai/docs/features/session-sticky-routing
	SessionID string                     `json:"session_id,omitempty"`
	Provider  *openRouterProviderRouting `json:"provider,omitempty"`
}

type openRouterReasoning struct {
	Effort string `json:"effort,omitempty"`
}

// buildRequest builds an OpenRouter-compatible request struct with nested
// reasoning format. The caller owns Stream and other provider-agnostic fields.
func (p *Provider) buildRequest(req *llm.Request, streaming bool) openRouterRequest {
	base := p.Base.ConvertRequest(req)
	if streaming {
		base = p.Base.ConvertStreamingRequest(req)
	}

	effort := p.Base.ReasoningEffortForModel(req.Model, req.ReasoningEffort)
	caps := llm.CapabilitiesForRequest(req, p.Base.Capabilities(req.Model))

	orReq := openRouterRequest{
		ChatCompletionRequest: base,
		SessionID:             req.SessionID,
	}

	// Inject per-model provider routing if configured.
	if routing, ok := p.modelRouting[req.Model]; ok {
		r := routing // copy
		orReq.Provider = &r
	}

	// Clear the top-level reasoning_effort since OpenRouter uses the nested format.
	orReq.ReasoningEffort = ""

	// Build the nested reasoning object.
	if effort != "" {
		if IsReasoningOff(effort) {
			if caps.ReasoningCaps().CanDisable {
				orReq.Reasoning = &openRouterReasoning{Effort: "none"}
			}
		} else {
			orReq.Reasoning = &openRouterReasoning{Effort: effort}
		}
	} else if caps.ReasoningCaps().Kind != "" && caps.ReasoningCaps().CanDisable {
		orReq.Reasoning = &openRouterReasoning{Effort: "none"}
	}

	return orReq
}

// buildAndMarshalRequest builds a non-streaming request and marshals it.
func (p *Provider) buildAndMarshalRequest(req *llm.Request) ([]byte, error) {
	body, err := json.Marshal(p.buildRequest(req, false))
	if err != nil {
		return nil, err
	}
	return p.Base.ApplyThinkingSignatures(body, req), nil
}

// buildAndMarshalRequestStream builds a streaming request and marshals it.
func (p *Provider) buildAndMarshalRequestStream(req *llm.Request) ([]byte, error) {
	orReq := p.buildRequest(req, true)
	orReq.Stream = true
	body, err := json.Marshal(orReq)
	if err != nil {
		return nil, err
	}
	return p.Base.ApplyThinkingSignatures(body, req), nil
}

// IsReasoningOff returns true if the effort value represents disabled reasoning.
func IsReasoningOff(effort string) bool {
	switch strings.ToLower(strings.TrimSpace(effort)) {
	case "", "off", "none", "disabled":
		return true
	}
	return false
}

// buildBlocks constructs ContentBlocks from flat OpenRouter message fields.
func (p *Provider) buildBlocks(content string, reasoning string, toolCalls []sashaoai.ToolCall) []llm.ContentBlock {
	var blocks []llm.ContentBlock
	if reasoning != "" {
		blocks = append(blocks, llm.ThinkingBlock{Thinking: reasoning})
	}
	if content != "" {
		blocks = append(blocks, llm.TextBlock{Text: content})
	}
	for _, tc := range toolCalls {
		blocks = append(blocks, llm.ToolCallBlock{
			ID:        tc.ID,
			Name:      tc.Function.Name,
			Arguments: tc.Function.Arguments,
			Type:      string(tc.Type),
		})
	}
	if len(blocks) == 0 {
		return nil
	}
	return blocks
}
