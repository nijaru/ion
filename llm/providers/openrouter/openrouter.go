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

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers/openai"
	sashaoai "github.com/sashabaranov/go-openai"
)

// Provider implements the llm.Provider interface for OpenRouter.
// It overrides Generate/Stream to use OpenRouter's nested reasoning format:
//
//	{ "reasoning": { "effort": "high" } }
//
// instead of OpenAI's top-level reasoning_effort string, which OpenRouter
// rejects with 422 for models that require the nested format.
type Provider struct {
	openai.Base
	httpClient *http.Client
	endpoint   string
	apiKey     string
	headers    map[string]string
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

	return &Provider{
		Base:       p.Base,
		httpClient: &http.Client{},
		endpoint:   strings.TrimRight(endpoint, "/"),
		apiKey:     apiKey,
		headers:    cfg.DefaultHeaders,
	}
}

func (p *Provider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	prepared, err := llm.PrepareRequestForCapabilities(req, p.Base.Capabilities(req.Model))
	if err != nil {
		return nil, err
	}

	body, err := p.buildRequestJSON(prepared)
	if err != nil {
		return nil, fmt.Errorf("openrouter: build request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, p.endpoint+"/chat/completions", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("openrouter: create request: %w", err)
	}
	p.setHeaders(httpReq)

	httpResp, err := p.httpClient.Do(httpReq)
	if err != nil {
		return nil, err
	}
	defer httpResp.Body.Close()

	respBody, err := io.ReadAll(httpResp.Body)
	if err != nil {
		return nil, fmt.Errorf("openrouter: read response: %w", err)
	}

	if httpResp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("openrouter: status %d: %s", httpResp.StatusCode, string(respBody))
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
		Content:   choice.Message.Content,
		Reasoning: choice.Message.ReasoningContent,
		Calls:     p.Base.ConvertToolCalls(choice.Message.ToolCalls),
		Usage:     usage,
	}, nil
}

func (p *Provider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	prepared, err := llm.PrepareRequestForCapabilities(req, p.Base.Capabilities(req.Model))
	if err != nil {
		return nil, err
	}

	body, err := p.buildRequestJSON(prepared)
	if err != nil {
		return nil, fmt.Errorf("openrouter: build request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, p.endpoint+"/chat/completions", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("openrouter: create request: %w", err)
	}
	p.setHeaders(httpReq)

	httpResp, err := p.httpClient.Do(httpReq)
	if err != nil {
		return nil, err
	}

	if httpResp.StatusCode != http.StatusOK {
		defer httpResp.Body.Close()
		respBody, _ := io.ReadAll(httpResp.Body)
		return nil, fmt.Errorf("openrouter: status %d: %s", httpResp.StatusCode, string(respBody))
	}

	return &openRouterStream{
		body:   httpResp.Body,
		reader: httpResp.Body,
	}, nil
}

func (p *Provider) setHeaders(req *http.Request) {
	req.Header.Set("Content-Type", "application/json")
	if p.apiKey != "" {
		req.Header.Set("Authorization", "Bearer "+p.apiKey)
	}
	req.Header.Set("User-Agent", "canto/0.0.0")
	for k, v := range p.headers {
		req.Header.Set(k, v)
	}
}

// openRouterRequest wraps the standard OpenAI request to add OpenRouter-specific
// fields. The Reasoning field uses OpenRouter's nested format.
type openRouterRequest struct {
	sashaoai.ChatCompletionRequest
	Reasoning *openRouterReasoning `json:"reasoning,omitempty"`
}

type openRouterReasoning struct {
	Effort string `json:"effort,omitempty"`
}

// buildRequestJSON converts an llm.Request into an OpenRouter-compatible JSON body.
func (p *Provider) buildRequestJSON(req *llm.Request) ([]byte, error) {
	base := p.Base.ConvertRequest(req)

	effort := req.ReasoningEffort
	caps := p.Base.Capabilities(req.Model)

	orReq := openRouterRequest{
		ChatCompletionRequest: base,
	}

	// Clear the top-level reasoning_effort since OpenRouter uses the nested format.
	orReq.ReasoningEffort = ""

	// Build the nested reasoning object.
	if effort != "" {
		if IsReasoningOff(effort) {
			// Only send the "off" effort if the model supports disabling reasoning.
			if caps.ReasoningCaps().CanDisable {
				orReq.Reasoning = &openRouterReasoning{Effort: "none"}
			}
		} else {
			orReq.Reasoning = &openRouterReasoning{Effort: effort}
		}
	} else if caps.ReasoningCaps().Kind != "" && caps.ReasoningCaps().CanDisable {
		// Model supports reasoning but no effort specified: default to "none" to
		// avoid unwanted reasoning charges on non-reasoning requests.
		orReq.Reasoning = &openRouterReasoning{Effort: "none"}
	}

	return json.Marshal(orReq)
}

// IsReasoningOff returns true if the effort value represents disabled reasoning.
func IsReasoningOff(effort string) bool {
	switch strings.ToLower(strings.TrimSpace(effort)) {
	case "", "off", "none", "disabled":
		return true
	}
	return false
}
