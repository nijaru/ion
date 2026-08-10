package openrouter

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestNewProviderDefaults(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{})

	if got, want := p.ID(), "openrouter"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://openrouter.ai/api/v1"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}

func TestNewProviderRespectsConfig(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		ID:          "openrouter-custom",
		APIEndpoint: "https://example.test/openrouter",
	})

	if got, want := p.ID(), "openrouter-custom"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://example.test/openrouter"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}

func TestBuildRequestJSON_NestedReasoningFormat(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	req := &llm.Request{
		Model:           "xiaomi/mimo-v2.5-pro",
		Messages:        []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		ReasoningEffort: "medium",
	}

	body, err := p.buildAndMarshalRequest(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	// Parse the raw JSON to verify the reasoning object is nested, not top-level.
	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	// Top-level reasoning_effort must NOT be present.
	if _, ok := raw["reasoning_effort"]; ok {
		t.Fatal("reasoning_effort should not be a top-level field")
	}

	// reasoning must be a nested object with effort.
	reasoningRaw, ok := raw["reasoning"]
	if !ok {
		t.Fatal("reasoning object missing from request")
	}
	reasoning, ok := reasoningRaw.(map[string]any)
	if !ok {
		t.Fatalf("reasoning is %T, want object", reasoningRaw)
	}
	if got, want := reasoning["effort"], "medium"; got != want {
		t.Fatalf("reasoning.effort = %v, want %v", got, want)
	}
}

func TestBuildRequestJSON_PreservesThinkingSignature(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{APIKey: "test-key"})
	req := &llm.Request{
		Model: "openai/gpt-oss",
		Messages: []llm.Message{{
			Role: llm.RoleAssistant,
			Blocks: llm.ContentBlocks{
				llm.ThinkingBlock{Thinking: "think first", Signature: "reasoning_content"},
				llm.TextBlock{Text: "answer"},
			},
		}},
	}

	body, err := p.buildAndMarshalRequest(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}
	if !strings.Contains(string(body), `"reasoning_content":"think first"`) {
		t.Fatalf("request body = %s", body)
	}
}

func TestBuildRequestJSON_NoReasoningWhenNotSpecified(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{APIKey: "test-key"})

	req := &llm.Request{
		Model:    "gpt-4",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	}

	body, err := p.buildAndMarshalRequest(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	if _, ok := raw["reasoning"]; ok {
		t.Fatal("reasoning should not be present for non-reasoning models")
	}
	if _, ok := raw["reasoning_effort"]; ok {
		t.Fatal("reasoning_effort should not be a top-level field")
	}
}

func TestBuildRequestJSON_ReasoningOffForReasoningModel(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	// No reasoning effort specified for a reasoning model: should default to "none"
	// to avoid unwanted reasoning charges.
	req := &llm.Request{
		Model:    "xiaomi/mimo-v2.5-pro",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	}

	body, err := p.buildAndMarshalRequest(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	reasoningRaw, ok := raw["reasoning"]
	if !ok {
		t.Fatal("reasoning object missing from request for reasoning model")
	}
	reasoning, ok := reasoningRaw.(map[string]any)
	if !ok {
		t.Fatalf("reasoning is %T, want object", reasoningRaw)
	}
	if got, want := reasoning["effort"], "none"; got != want {
		t.Fatalf("reasoning.effort = %v, want %v", got, want)
	}
}

func TestIsReasoningOff(t *testing.T) {
	tests := []struct {
		input string
		want  bool
	}{
		{"", true},
		{"off", true},
		{"none", true},
		{"disabled", true},
		{"OFF", true},
		{"None", true},
		{"low", false},
		{"medium", false},
		{"high", false},
	}
	for _, tt := range tests {
		if got := IsReasoningOff(tt.input); got != tt.want {
			t.Errorf("IsReasoningOff(%q) = %v, want %v", tt.input, got, tt.want)
		}
	}
}

func TestBuildRequestJSON_StripsTopLevelReasoningEffort(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	// Build the request - the provider should strip reasoning_effort and use nested reasoning.
	req := &llm.Request{
		Model:           "xiaomi/mimo-v2.5-pro",
		Messages:        []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		ReasoningEffort: "high",
	}

	body, err := p.buildAndMarshalRequest(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	if _, ok := raw["reasoning_effort"]; ok {
		t.Fatal("reasoning_effort should not be a top-level field")
	}

	reasoningRaw, ok := raw["reasoning"]
	if !ok {
		t.Fatal("reasoning object missing")
	}
	reasoning, ok := reasoningRaw.(map[string]any)
	if !ok {
		t.Fatalf("reasoning is %T, want object", reasoningRaw)
	}
	if got, want := reasoning["effort"], "high"; got != want {
		t.Fatalf("reasoning.effort = %v, want %v", got, want)
	}
}

func TestOpenRouterProviderUsesBaseCapabilities(t *testing.T) {
	// Verify that the OpenRouter provider correctly delegates capabilities
	// to the underlying Base provider.
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	caps := p.Capabilities("xiaomi/mimo-v2.5-pro")
	if caps.Temperature {
		t.Fatal("mimo should not have temperature enabled")
	}
	if !caps.SupportsReasoningEffort("high") {
		t.Fatal("mimo should support reasoning effort high")
	}
}

func TestStreamRequestSetsStreamTrue(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "test/model",
			Capabilities: &llm.Capabilities{
				Streaming: true,
			},
		}},
	})

	req := &llm.Request{
		Model:    "test/model",
		Messages: []llm.Message{{Role: "user", Content: "hi"}},
	}

	body, err := p.buildAndMarshalRequestStream(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var parsed map[string]any
	if err := json.Unmarshal(body, &parsed); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	stream, ok := parsed["stream"]
	if !ok || stream != true {
		t.Fatalf("stream = %v (present=%v), want true", stream, ok)
	}
	if _, ok := parsed["stream_options"]; !ok {
		t.Fatal("stream_options missing from streaming request")
	}
}

func TestStreamUsageChunkIncludesCalculatedCost(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID:           "test/model",
			CostPer1MIn:  1,
			CostPer1MOut: 2,
			Capabilities: &llm.Capabilities{Streaming: true},
		}},
	})
	stream := &openRouterStream{
		reader: strings.NewReader(
			`data: {"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":500,"total_tokens":1500}}` + "\n" +
				`data: [DONE]` + "\n",
		),
		provider: p,
		ctx:      context.Background(),
		model:    "test/model",
	}

	chunk, ok := stream.Next()
	if !ok {
		t.Fatal("Next returned ok=false, want usage chunk")
	}
	if chunk.Usage == nil {
		t.Fatal("Usage is nil")
	}
	if got, want := chunk.Usage.InputTokens, 1000; got != want {
		t.Fatalf("InputTokens = %d, want %d", got, want)
	}
	if got, want := chunk.Usage.OutputTokens, 500; got != want {
		t.Fatalf("OutputTokens = %d, want %d", got, want)
	}
	if got, want := chunk.Usage.Cost, 0.002; got != want {
		t.Fatalf("Cost = %.6f, want %.6f", got, want)
	}
}

func TestStreamUsageChunkPrefersOpenRouterRawCost(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID:           "test/model",
			CostPer1MIn:  1,
			CostPer1MOut: 2,
			Capabilities: &llm.Capabilities{Streaming: true},
		}},
	})
	stream := &openRouterStream{
		reader: strings.NewReader(
			`data: {"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":500,"total_tokens":1500,"cost":0.0042}}` + "\n" +
				`data: [DONE]` + "\n",
		),
		provider: p,
		ctx:      context.Background(),
		model:    "test/model",
	}

	chunk, ok := stream.Next()
	if !ok {
		t.Fatal("Next returned ok=false, want usage chunk")
	}
	if chunk.Usage == nil {
		t.Fatal("Usage is nil")
	}
	if got, want := chunk.Usage.Cost, 0.0042; got != want {
		t.Fatalf("Cost = %.6f, want %.6f", got, want)
	}
}

func TestStreamAssemblesFragmentedToolCall(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/chat/completions" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		events := []string{
			"{\"id\":\"resp-1\",\"model\":\"actual-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"ion_live_echo\",\"arguments\":\"{\\\"text\\\":\\\"live-\"}}]}}]}",
			"{\"id\":\"resp-1\",\"model\":\"actual-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"check\\\"}\"}}]}}]}",
			"{\"id\":\"resp-1\",\"model\":\"actual-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}",
			"{\"id\":\"resp-1\",\"model\":\"actual-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}",
		}
		for _, event := range events {
			_, _ = fmt.Fprintln(w, "data: "+event)
			_, _ = fmt.Fprintln(w)
		}
		_, _ = fmt.Fprintln(w, "data: [DONE]")
		_, _ = fmt.Fprintln(w)
	}))
	defer server.Close()

	provider := NewProvider(llm.ProviderConfig{
		APIKey:      "test-key",
		APIEndpoint: server.URL + "/v1",
		Models: []llm.Model{{
			ID: "test/model",
			Capabilities: &llm.Capabilities{
				Streaming: true,
				Tools:     true,
			},
		}},
	})
	stream, err := provider.Stream(t.Context(), &llm.Request{
		Model:    "test/model",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "call the tool"}},
		Tools: []*llm.Spec{{
			Name:        "ion_live_echo",
			Description: "echo text",
			Parameters:  map[string]any{"type": "object"},
		}},
	})
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	response, err := llm.GenerateFromStream(stream)
	if err != nil {
		t.Fatalf("GenerateFromStream: %v", err)
	}
	calls := response.ToolCalls()
	if len(calls) != 1 {
		t.Fatalf("tool calls = %#v, want one assembled call", calls)
	}
	call := calls[0]
	if call.ID != "call-1" || call.Function.Name != "ion_live_echo" {
		t.Fatalf("assembled call identity = %#v", call)
	}
	if call.Function.Arguments != `{"text":"live-check"}` {
		t.Fatalf("assembled call arguments = %q, want complete JSON", call.Function.Arguments)
	}
	if response.StopReason != llm.StopReasonToolUse {
		t.Fatalf("stop reason = %q, want tool use", response.StopReason)
	}
	if response.ResponseID != "resp-1" || response.ResponseModel != "actual-model" {
		t.Fatalf("response metadata = id %q model %q", response.ResponseID, response.ResponseModel)
	}
	if response.Usage.TotalTokens != 8 {
		t.Fatalf("usage total = %d, want 8", response.Usage.TotalTokens)
	}
}

func TestStreamMalformedEventIsReported(t *testing.T) {
	stream := &openRouterStream{reader: strings.NewReader("data: {not-json}\n")}
	if chunk, ok := stream.Next(); ok || chunk != nil {
		t.Fatalf("Next() = (%#v, %v), want terminal error", chunk, ok)
	}
	if stream.Err() == nil {
		t.Fatal("Err() = nil, want malformed SSE error")
	}
}

func TestGenerateRequestDoesNotSetStreamTrue(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "test/model",
			Capabilities: &llm.Capabilities{
				Streaming: true,
			},
		}},
	})

	req := &llm.Request{
		Model:    "test/model",
		Messages: []llm.Message{{Role: "user", Content: "hi"}},
	}

	body, err := p.buildAndMarshalRequest(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var parsed map[string]any
	if err := json.Unmarshal(body, &parsed); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	stream, exists := parsed["stream"]
	if exists && stream == true {
		t.Fatal("Generate request should not set stream: true")
	}
	if _, exists := parsed["stream_options"]; exists {
		t.Fatal("Generate request should not set stream_options")
	}
}
