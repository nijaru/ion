package openai

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
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

func TestCompatibleProviderUsesScopedCapabilityRegistry(t *testing.T) {
	registry := llm.NewRegistry()
	registry.Register(llm.ModelDef{
		Pattern: "*custom-reasoning*",
		Preset:  llm.PresetReasoning,
	})
	p := NewCompatibleProvider(
		llm.ProviderConfig{ModelRegistry: registry},
		CompatibleSpec{ID: "local-api", DefaultAPIEndpoint: "http://localhost:8080/v1"},
	)

	caps := p.Capabilities("vendor/custom-reasoning-model")
	if caps.Reasoning.Kind != llm.ReasoningKindEffort || !caps.SupportsReasoningEffort("high") {
		t.Fatalf("scoped capability registry returned %#v, want reasoning capabilities", caps)
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

func TestConvertRequestAppliesCompatibilityRoleAndToolBridge(t *testing.T) {
	b := &Base{
		Config: llm.ProviderConfig{ID: "compatible"},
		Compat: &llm.ProviderCompat{
			SupportsDeveloperRole:            false,
			RequiresAssistantAfterToolResult: true,
		},
	}
	converted := b.ConvertRequest(&llm.Request{Messages: []llm.Message{
		{Role: llm.RoleDeveloper, Content: "developer instructions"},
		{Role: llm.RoleTool, ToolID: "call-1", Content: "tool output"},
		{Role: llm.RoleUser, Content: "continue"},
	}})
	if len(converted.Messages) != 4 {
		t.Fatalf("messages = %d, want developer fallback plus bridge", len(converted.Messages))
	}
	if got := converted.Messages[0].Role; got != string(llm.RoleSystem) {
		t.Fatalf("developer role = %q, want system fallback", got)
	}
	if got := converted.Messages[2].Content; got != "I have processed the tool results." ||
		converted.Messages[2].Role != string(llm.RoleAssistant) {
		t.Fatalf("bridge message = %#v, want assistant bridge", converted.Messages[2])
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
	if len(converted.Messages) != 2 {
		t.Fatalf("messages = %d, want tool result plus attached image user message", len(converted.Messages))
	}
	toolMsg := converted.Messages[0]
	if toolMsg.Content != "Read image file [image/png]" {
		t.Fatalf("tool content = %q, want text-only tool result", toolMsg.Content)
	}
	if toolMsg.ToolCallID != "call-1" {
		t.Fatalf("tool call id = %q, want call-1", toolMsg.ToolCallID)
	}
	if len(toolMsg.MultiContent) != 0 {
		t.Fatalf("tool multi-content = %+v, want no image parts", toolMsg.MultiContent)
	}
	msg := converted.Messages[1]
	if msg.Role != string(llm.RoleUser) || len(msg.MultiContent) != 2 {
		t.Fatalf("attached image message = %+v, want user text and image", msg)
	}
	if got := msg.MultiContent[0].Text; got != "Attached image(s) from tool result:" {
		t.Fatalf("text part = %q", got)
	}
	image := msg.MultiContent[1].ImageURL
	if image == nil || image.URL != "data:image/png;base64,aW1hZ2U=" {
		t.Fatalf("image part = %+v", msg.MultiContent[1])
	}

	raw, err := json.Marshal(toolMsg)
	if err != nil {
		t.Fatalf("marshal message: %v", err)
	}
	if got := string(raw); !containsAll(
		got,
		`"content":"Read image file [image/png]"`,
		`"tool_call_id":"call-1"`,
	) {
		t.Fatalf("marshaled tool message = %s", got)
	}

	raw, err = json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal image message: %v", err)
	}
	if !containsAll(string(raw), `"role":"user"`, `"content":[`, `"image_url"`) {
		t.Fatalf("marshaled image message = %s", raw)
	}
}

func TestConvertRequestNormalizesToolCallIDsForWireLimit(t *testing.T) {
	longID := strings.Repeat("a", 48)
	converted := (&Base{Config: llm.ProviderConfig{ID: "openai"}}).ConvertRequest(&llm.Request{
		Messages: []llm.Message{
			{
				Role: llm.RoleAssistant,
				Calls: []llm.Call{{
					ID:   longID,
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "lookup", Arguments: `{}`},
				}},
			},
			{Role: llm.RoleTool, ToolID: longID, Content: "result"},
		},
	})
	if got := converted.Messages[0].ToolCalls[0].ID; len(got) != 40 {
		t.Fatalf("assistant tool call id length = %d, want 40", len(got))
	}
	if got := converted.Messages[1].ToolCallID; got != converted.Messages[0].ToolCalls[0].ID {
		t.Fatalf("tool result id = %q, want %q", got, converted.Messages[0].ToolCalls[0].ID)
	}
}

func TestConvertRequestAppliesModelCompatibilityOverrides(t *testing.T) {
	falseValue := false
	p := NewProvider(llm.ProviderConfig{
		Models: []llm.Model{{
			ID: "custom-model",
			Capabilities: &llm.Capabilities{
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"low", "balanced"},
					CanDisable: true,
				},
			},
			ThinkingLevelMap: map[string]string{"low": "balanced", "off": "none"},
			Compat: &llm.CompatFlags{
				SupportsDeveloperRole:  &falseValue,
				SupportsStreamOptions:  &falseValue,
				RequiresToolResultName: boolPointer(true),
				MaxTokensField:         "max_tokens",
			},
		}},
	})
	converted := p.ConvertRequest(&llm.Request{
		Model:           "custom-model",
		MaxTokens:       123,
		ReasoningEffort: "low",
		Messages: []llm.Message{
			{Role: llm.RoleDeveloper, Content: "instructions"},
			{Role: llm.RoleTool, ToolID: "call-1", Name: "lookup", Content: "result"},
		},
	})
	if converted.StreamOptions != nil {
		t.Fatal("stream options present on non-streaming request")
	}
	streaming := p.ConvertStreamingRequest(&llm.Request{Model: "custom-model"})
	if streaming.StreamOptions != nil {
		t.Fatal("stream options present despite model override")
	}
	if converted.MaxTokens != 123 || converted.MaxCompletionTokens != 0 {
		t.Fatalf(
			"max token fields = max_tokens:%d max_completion_tokens:%d",
			converted.MaxTokens,
			converted.MaxCompletionTokens,
		)
	}
	if got := converted.Messages[0].Role; got != string(llm.RoleSystem) {
		t.Fatalf("developer role = %q, want system fallback", got)
	}
	if got := converted.Messages[1].Name; got != "lookup" {
		t.Fatalf("tool result name = %q, want lookup", got)
	}
	if got := converted.ReasoningEffort; got != "balanced" {
		t.Fatalf("request reasoning field = %q, want mapped balanced", got)
	}
	off := p.ConvertRequest(&llm.Request{Model: "custom-model"})
	if got := off.ReasoningEffort; got != "none" {
		t.Fatalf("default reasoning field = %q, want mapped none", got)
	}
	if got := converted.ChatTemplateKwargs; got != nil {
		t.Fatalf("chat template kwargs = %#v, want nil for default OpenAI format", got)
	}
	if got := converted.Messages; len(got) != 2 {
		t.Fatalf("messages = %d, want 2", len(got))
	}
}

func boolPointer(value bool) *bool { return &value }

func TestConvertRequestPreservesReasoningForCompatibleReplay(t *testing.T) {
	b := &Base{
		Config: llm.ProviderConfig{ID: "deepseek"},
		Compat: &llm.ProviderCompat{
			RequiresReasoningContentOnAssistantMessages: true,
		},
	}
	converted := b.ConvertRequest(&llm.Request{
		Model: "deepseek-chat",
		Messages: []llm.Message{{
			Role:      llm.RoleAssistant,
			Content:   "answer",
			Reasoning: "think first",
		}},
	})
	if got := converted.Messages[0].ReasoningContent; got != "think first" {
		t.Fatalf("reasoning_content = %q, want think first", got)
	}

	b.Compat = &llm.ProviderCompat{RequiresThinkingAsText: true}
	converted = b.ConvertRequest(&llm.Request{
		Model: "local-reasoning",
		Messages: []llm.Message{{
			Role:      llm.RoleAssistant,
			Content:   "answer",
			Reasoning: "think first",
		}},
	})
	if got, want := converted.Messages[0].Content, "think first\n\nanswer"; got != want {
		t.Fatalf("thinking-as-text content = %q, want %q", got, want)
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
			"model": "actual-model",
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
	if resp.TextContent() != "done" {
		t.Fatalf("content = %q, want done", resp.TextContent())
	}
	if resp.ReasoningContent() != "thinking through it" {
		t.Fatalf("reasoning = %q, want thinking through it", resp.ReasoningContent())
	}
	if resp.ResponseID != "chatcmpl-test" || resp.ResponseModel != "actual-model" {
		t.Fatalf("response metadata = id %q model %q", resp.ResponseID, resp.ResponseModel)
	}
	if resp.StopReason != llm.StopReasonStop {
		t.Fatalf("stop reason = %q, want stop", resp.StopReason)
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
				`data: {"id":"chatcmpl-test","model":"actual-model","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"reasoning_content":"thinking "}}]}

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
	if resp.TextContent() != "done" {
		t.Fatalf("content = %q, want done", resp.TextContent())
	}
	if resp.ReasoningContent() != "thinking through it" {
		t.Fatalf("reasoning = %q, want thinking through it", resp.ReasoningContent())
	}
	if resp.Usage.TotalTokens != 7 {
		t.Fatalf("total tokens = %d, want 7", resp.Usage.TotalTokens)
	}
	if resp.ResponseID != "chatcmpl-test" || resp.ResponseModel != "actual-model" {
		t.Fatalf("response metadata = id %q model %q", resp.ResponseID, resp.ResponseModel)
	}
	if resp.StopReason != llm.StopReasonStop {
		t.Fatalf("stop reason = %q, want stop", resp.StopReason)
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
