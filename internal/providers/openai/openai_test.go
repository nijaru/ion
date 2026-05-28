package openai

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

func TestNewProviderDefaults(t *testing.T) {
	reasoningCaps := llm.Capabilities{
		Streaming:  true,
		Tools:      true,
		SystemRole: llm.RoleDeveloper,
		Reasoning: llm.ReasoningCapabilities{
			Kind:       llm.ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high"},
			CanDisable: true,
		},
	}
	p := NewProvider(llm.ProviderConfig{
		Models: []llm.Model{
			{ID: "o4-mini", Capabilities: &reasoningCaps},
		},
	})

	if got, want := p.ID(), "openai"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://api.openai.com/v1"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
	if caps := p.Capabilities("o4-mini"); caps.Reasoning.Kind != llm.ReasoningKindEffort {
		t.Fatal("expected OpenAI reasoning model capability defaults")
	} else if !caps.SupportsReasoningEffort("high") || !caps.SupportsReasoningEffort("none") {
		t.Fatalf("unexpected OpenAI reasoning capabilities: %#v", caps.Reasoning)
	}
}

func TestCompatibleProviderDefaultsToNoReasoningCaps(t *testing.T) {
	reasoningCaps := llm.Capabilities{
		Streaming: true,
		Tools:     true,
		Reasoning: llm.ReasoningCapabilities{
			Kind:       llm.ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high"},
			CanDisable: true,
		},
	}
	p := NewCompatibleProvider(llm.ProviderConfig{
		ID: "local-api",
		Models: []llm.Model{
			{ID: "xiaomi/mimo-v2.5-pro", Capabilities: &reasoningCaps},
			{ID: "deepseek/deepseek-r1", Capabilities: &reasoningCaps},
			{ID: "o3-pro", Capabilities: &reasoningCaps},
		},
	}, CompatibleSpec{
		ID:                 "local-api",
		DefaultAPIEndpoint: "http://localhost:8080/v1",
	})

	if caps := p.Capabilities("gpt-4o"); caps.Reasoning.Kind != llm.ReasoningKindNone ||
		caps.SupportsReasoningEffort("high") {
		t.Fatalf("compatible provider caps = %#v, want no reasoning by default for gpt-4o", caps)
	}

	// 1. Configured reasoning models should resolve correctly out of the box
	for _, model := range []string{
		"xiaomi/mimo-v2.5-pro",
		"deepseek/deepseek-r1",
		"o3-pro",
	} {
		if caps := p.Capabilities(model); caps.Reasoning.Kind != llm.ReasoningKindEffort ||
			!caps.SupportsReasoningEffort("high") {
			t.Fatalf(
				"compatible provider caps = %#v, want reasoning capabilities for %s",
				caps,
				model,
			)
		}
	}

	// 2. Unregistered models (like r3/r2 variants) now default to standard chat deterministically
	for _, model := range []string{
		"deepseek/deepseek-r2-preview",
		"llama-3.3-r3",
	} {
		if caps := p.Capabilities(model); caps.Reasoning.Kind != llm.ReasoningKindNone {
			t.Fatalf(
				"expected unregistered model %s to default to standard chat, got %s reasoning kind",
				model,
				caps.Reasoning.Kind,
			)
		}
	}
}

func TestNewProviderRespectsConfig(t *testing.T) {
	models := []llm.Model{{ID: "custom"}}
	p := NewProvider(llm.ProviderConfig{
		ID:          "openai-custom",
		APIEndpoint: "https://example.test/v1",
		Models:      models,
	})

	if got, want := p.ID(), "openai-custom"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://example.test/v1"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
	gotModels, err := p.Models(t.Context())
	if err != nil {
		t.Fatalf("Models: %v", err)
	}
	if len(gotModels) != 1 || gotModels[0].ID != "custom" {
		t.Fatalf("models = %#v, want custom", gotModels)
	}
}

func TestConvertRequestPreservesImageParts(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{})

	req := &llm.Request{
		Model: "gpt-test",
		Messages: []llm.Message{{
			Role:    llm.RoleTool,
			Content: "Read image file [image/png]",
			Parts: []llm.ContentPart{
				llm.TextPart("Read image file [image/png]"),
				llm.ImagePart("image/png", "aW1hZ2U="),
			},
			ToolID: "call-1",
			Name:   "read",
		}},
	}

	converted := p.ConvertRequest(req)
	if len(converted.Messages) != 1 {
		t.Fatalf("messages = %d, want 1", len(converted.Messages))
	}
	msg := converted.Messages[0]
	if msg.Content != "" {
		t.Fatalf("content = %q, want empty when multi-content is set", msg.Content)
	}
	if msg.ToolCallID != "call-1" {
		t.Fatalf("tool call id = %q, want call-1", msg.ToolCallID)
	}
	if len(msg.MultiContent) != 2 {
		t.Fatalf("multi-content = %+v, want text and image", msg.MultiContent)
	}
	if got := msg.MultiContent[0].Text; got != "Read image file [image/png]" {
		t.Fatalf("text part = %q", got)
	}
	image := msg.MultiContent[1].ImageURL
	if image == nil || image.URL != "data:image/png;base64,aW1hZ2U=" {
		t.Fatalf("image part = %+v", msg.MultiContent[1])
	}

	raw, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal message: %v", err)
	}
	if got := string(raw); !containsAll(
		got,
		`"content":[`,
		`"image_url"`,
		`"tool_call_id":"call-1"`,
	) {
		t.Fatalf("marshaled message = %s", got)
	}
}

func containsAll(s string, needles ...string) bool {
	for _, needle := range needles {
		if !strings.Contains(s, needle) {
			return false
		}
	}
	return true
}

func TestGeneratePreservesReasoningContent(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/chat/completions" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{
			"id": "chatcmpl-test",
			"object": "chat.completion",
			"choices": [{
				"index": 0,
				"message": {
					"role": "assistant",
					"reasoning_content": "thinking through it",
					"content": "done"
				},
				"finish_reason": "stop"
			}],
			"usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
		}`))
	}))
	defer server.Close()

	p := NewCompatibleProvider(llm.ProviderConfig{
		ID:          "local-api",
		APIEndpoint: server.URL + "/v1",
		APIKey:      "test",
	}, CompatibleSpec{ID: "local-api"})

	resp, err := p.Generate(t.Context(), &llm.Request{
		Model:    "test-model",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	})
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	if resp.Content != "done" {
		t.Fatalf("content = %q, want done", resp.Content)
	}
	if resp.Reasoning != "thinking through it" {
		t.Fatalf("reasoning = %q, want thinking through it", resp.Reasoning)
	}
}

func TestStreamPreservesReasoningContent(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/chat/completions" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(
			[]byte(
				`data: {"id":"chatcmpl-test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"reasoning_content":"thinking "}}]}

data: {"id":"chatcmpl-test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"reasoning_content":"through it"}}]}

data: {"id":"chatcmpl-test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"done"}}]}

data: {"id":"chatcmpl-test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-test","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}

data: [DONE]

`,
			),
		)
	}))
	defer server.Close()

	p := NewCompatibleProvider(llm.ProviderConfig{
		ID:          "local-api",
		APIEndpoint: server.URL + "/v1",
		APIKey:      "test",
	}, CompatibleSpec{ID: "local-api"})

	stream, err := p.Stream(t.Context(), &llm.Request{
		Model:    "test-model",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	})
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	resp, err := llm.GenerateFromStream(stream)
	if err != nil {
		t.Fatalf("GenerateFromStream: %v", err)
	}
	if resp.Content != "done" {
		t.Fatalf("content = %q, want done", resp.Content)
	}
	if resp.Reasoning != "thinking through it" {
		t.Fatalf("reasoning = %q, want thinking through it", resp.Reasoning)
	}
	if resp.Usage.TotalTokens != 7 {
		t.Fatalf("total tokens = %d, want 7", resp.Usage.TotalTokens)
	}
}

func TestConvertRequestBooleanReasoningUsesChatTemplateKwargs(t *testing.T) {
	p := NewCompatibleProvider(llm.ProviderConfig{ID: "local-api"}, CompatibleSpec{
		ID:                 "local-api",
		DefaultAPIEndpoint: "http://localhost:8080/v1",
		ModelCaps: map[string]llm.Capabilities{
			"qwen": {
				Streaming: true,
				Tools:     true,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindBoolean,
					CanDisable: true,
				},
			},
		},
	})

	enabled := p.ConvertRequest(&llm.Request{
		Model:           "qwen",
		ReasoningEffort: "high",
		Messages:        []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	})
	if enabled.ReasoningEffort != "" {
		t.Fatalf("reasoning effort = %q, want empty", enabled.ReasoningEffort)
	}
	if enabled.ChatTemplateKwargs["enable_thinking"] != true ||
		enabled.ChatTemplateKwargs["preserve_thinking"] != true {
		t.Fatalf("enabled chat template kwargs = %#v", enabled.ChatTemplateKwargs)
	}

	disabled := p.ConvertRequest(&llm.Request{
		Model:           "qwen",
		ReasoningEffort: "none",
		Messages:        []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	})
	if disabled.ChatTemplateKwargs["enable_thinking"] != false ||
		disabled.ChatTemplateKwargs["preserve_thinking"] != true {
		t.Fatalf("disabled chat template kwargs = %#v", disabled.ChatTemplateKwargs)
	}
}

func TestIsContextOverflowMessage(t *testing.T) {
	if !isContextOverflowMessage("This model's context window has too many TOKENS") {
		t.Fatal("expected mixed-case context/token message to match")
	}
	if isContextOverflowMessage("temporary server overload") {
		t.Fatal("expected unrelated message not to match")
	}
}
