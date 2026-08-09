package openai

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestAddMissingReasoningContent(t *testing.T) {
	body := []byte(`{"model":"deepseek-reasoner","messages":[{"role":"assistant","content":"old"},{"role":"user","content":"next"}]}`)
	repaired, changed := addMissingReasoningContent(body)
	if !changed {
		t.Fatal("request was not repaired")
	}
	text := string(repaired)
	if !strings.Contains(text, `"reasoning_content":""`) {
		t.Fatalf("repaired request = %s", text)
	}

	unchanged, changed := addMissingReasoningContent([]byte(`{"messages":[{"role":"assistant","reasoning_content":"kept"}]}`))
	if changed || string(unchanged) != `{"messages":[{"role":"assistant","reasoning_content":"kept"}]}` {
		t.Fatalf("existing reasoning field changed: %s (changed=%v)", unchanged, changed)
	}
}

func TestDeepSeekRequestIncludesEmptyReplayReasoningField(t *testing.T) {
	transport := &captureRequestTransport{}
	reasoningCaps := llm.Capabilities{
		Streaming: true,
		Reasoning: llm.ReasoningCapabilities{Kind: llm.ReasoningKindEffort, CanDisable: true},
	}
	provider := NewProvider(llm.ProviderConfig{
		ID:          "deepseek",
		APIKey:      "test",
		APIEndpoint: "https://example.test/v1",
		Models: []llm.Model{{
			ID:           "deepseek-reasoner",
			Capabilities: &reasoningCaps,
		}},
	})
	stream, err := provider.Stream(t.Context(), &llm.Request{
		Model: "deepseek-reasoner",
		Messages: []llm.Message{
			{Role: llm.RoleAssistant, Content: "previous answer"},
			{Role: llm.RoleUser, Content: "continue"},
		},
		Transport: transport,
	})
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	if _, ok := stream.Next(); !ok {
		t.Fatal("stream ended before response")
	}
	_ = stream.Close()
	if !strings.Contains(string(transport.body), `"reasoning_content":""`) {
		t.Fatalf("request body = %s", transport.body)
	}
}
