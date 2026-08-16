package openaicodex

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"

	"github.com/nijaru/ion/llm"
)

const defaultEndpoint = "https://chatgpt.com/backend-api"

// Provider speaks the ChatGPT Codex Responses API. It is deliberately separate
// from the public OpenAI Chat Completions adapter: Codex uses a different URL,
// account header, request envelope, and event stream.
type Provider struct {
	config llm.ProviderConfig
	client *http.Client
}

func NewProvider(cfg llm.ProviderConfig) *Provider {
	if strings.TrimSpace(cfg.APIEndpoint) == "" {
		cfg.APIEndpoint = defaultEndpoint
	}
	return &Provider{config: cfg, client: &http.Client{}}
}

func (p *Provider) ID() string { return "openai-codex" }

func (p *Provider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	stream, err := p.Stream(ctx, req)
	if err != nil {
		return nil, err
	}
	return llm.GenerateFromStream(stream)
}

func (p *Provider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	if req == nil {
		return nil, errors.New("openai-codex: request is nil")
	}
	if err := llm.ValidateRequest(req); err != nil {
		return nil, err
	}
	caps := llm.CapabilitiesForRequest(req, p.Capabilities(req.Model))
	prepared := req.Clone()
	prepared.CapabilitySnapshot = &caps
	body, err := json.Marshal(buildRequest(prepared))
	if err != nil {
		return nil, fmt.Errorf("openai-codex: encode request: %w", err)
	}
	request, err := http.NewRequestWithContext(
		ctx,
		http.MethodPost,
		codexEndpoint(p.config.APIEndpoint),
		bytes.NewReader(body),
	)
	if err != nil {
		return nil, fmt.Errorf("openai-codex: create request: %w", err)
	}
	request.Header.Set("Authorization", "Bearer "+p.config.APIKey)
	request.Header.Set("chatgpt-account-id", p.config.AccountID)
	request.Header.Set("originator", "ion")
	request.Header.Set("OpenAI-Beta", "responses=experimental")
	request.Header.Set("Accept", "text/event-stream")
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("User-Agent", "ion/0.0.0")
	if prepared.SessionID != "" {
		request.Header.Set("session-id", prepared.SessionID)
		request.Header.Set("x-client-request-id", prepared.SessionID)
	}
	for key, value := range p.config.DefaultHeaders {
		request.Header.Set(key, value)
	}
	for key, value := range prepared.Headers {
		request.Header.Set(key, value)
	}

	client := p.client
	if prepared.Transport != nil {
		client = &http.Client{Transport: prepared.Transport}
	}
	response, err := client.Do(request)
	if err != nil {
		return nil, err
	}
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		defer response.Body.Close()
		message, _ := io.ReadAll(io.LimitReader(response.Body, 64<<10))
		return nil, fmt.Errorf("openai-codex: HTTP %d: %s", response.StatusCode, strings.TrimSpace(string(message)))
	}
	return newStream(response.Body, prepared.Model), nil
}

func (p *Provider) Models(_ context.Context) ([]llm.Model, error) {
	return append([]llm.Model(nil), p.config.Models...), nil
}

func (p *Provider) CountTokens(_ context.Context, _ string, messages []llm.Message) (int, error) {
	total := 3
	for _, message := range messages {
		total += 4 + (len(message.TextContent())+3)/4
		for _, call := range message.BlocksToolCalls() {
			total += (len(call.Function.Name) + len(call.Function.Arguments) + 3) / 4
		}
	}
	return total, nil
}

func (p *Provider) Cost(_ context.Context, model string, usage llm.Usage) float64 {
	for _, configured := range p.config.Models {
		if configured.ID == model {
			return float64(usage.InputTokens)*configured.CostPer1MIn/1_000_000 +
				float64(usage.OutputTokens)*configured.CostPer1MOut/1_000_000
		}
	}
	return 0
}

func (p *Provider) Capabilities(_ string) llm.Capabilities {
	return llm.Capabilities{
		Streaming:       true,
		Tools:           true,
		Temperature:     false,
		SystemRole:      llm.RoleSystem,
		InputModalities: []string{"text", "image"},
		Reasoning: llm.ReasoningCapabilities{
			Kind:       llm.ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high", "xhigh", "max"},
			CanDisable: false,
		},
	}
}

func (p *Provider) IsTransient(err error) bool {
	message := strings.ToLower(errString(err))
	return strings.Contains(message, "http 408") ||
		strings.Contains(message, "http 429") ||
		strings.Contains(message, "http 500") ||
		strings.Contains(message, "http 502") ||
		strings.Contains(message, "http 503") ||
		strings.Contains(message, "http 504")
}

func (p *Provider) IsContextOverflow(err error) bool {
	message := strings.ToLower(errString(err))
	return strings.Contains(message, "context") &&
		(strings.Contains(message, "length") || strings.Contains(message, "token"))
}

func codexEndpoint(baseURL string) string {
	baseURL = strings.TrimRight(strings.TrimSpace(baseURL), "/")
	if strings.HasSuffix(baseURL, "/codex/responses") {
		return baseURL
	}
	if strings.HasSuffix(baseURL, "/codex") {
		return baseURL + "/responses"
	}
	return baseURL + "/codex/responses"
}

func errString(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

var _ llm.Provider = (*Provider)(nil)
