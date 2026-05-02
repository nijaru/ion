package canto

import (
	"testing"

	"github.com/nijaru/canto/llm"
)

func TestProviderRequestObserverCopiesRequest(t *testing.T) {
	var got *llm.Request
	restore := SetProviderRequestObserverForTest(func(provider string, req *llm.Request) {
		if provider != "test-provider" {
			t.Fatalf("provider = %q, want test-provider", provider)
		}
		got = req
	})
	t.Cleanup(restore)

	original := &llm.Request{
		Model: "test-model",
		Messages: []llm.Message{{
			Role:           llm.RoleAssistant,
			Content:        "answer",
			ThinkingBlocks: []llm.ThinkingBlock{{Type: "thinking", Thinking: "step"}},
			Calls: []llm.Call{{
				ID:   "call-1",
				Type: "function",
				Function: struct {
					Name      string `json:"name"`
					Arguments string `json:"arguments"`
				}{Name: "read", Arguments: `{"path":"README.md"}`},
			}},
			CacheControl: &llm.CacheControl{Type: "ephemeral"},
		}},
		Tools: []*llm.Spec{{
			Name: "read",
			Parameters: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"path": map[string]any{
						"type": "string",
						"enum": []string{"README.md"},
					},
				},
			},
			CacheControl: &llm.CacheControl{Type: "ephemeral"},
		}},
		ResponseFormat: &llm.ResponseFormat{
			Type: llm.ResponseFormatJSONSchema,
			Schema: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"ok": map[string]any{"type": "boolean"},
				},
			},
		},
		CachePrefixMessages: 1,
	}

	notifyProviderRequest("test-provider", original)
	if got == nil {
		t.Fatal("observer did not receive request")
	}

	original.Messages[0].ThinkingBlocks[0].Thinking = "mutated"
	original.Messages[0].Calls[0].ID = "mutated"
	original.Messages[0].CacheControl.Type = "mutated"
	original.Tools[0].CacheControl.Type = "mutated"
	toolParams := original.Tools[0].Parameters.(map[string]any)
	pathSchema := toolParams["properties"].(map[string]any)["path"].(map[string]any)
	pathSchema["type"] = "number"
	pathSchema["enum"].([]string)[0] = "mutated.md"
	formatOK := original.ResponseFormat.Schema["properties"].(map[string]any)["ok"].(map[string]any)
	formatOK["type"] = "string"

	gotToolParams := got.Tools[0].Parameters.(map[string]any)
	gotPathSchema := gotToolParams["properties"].(map[string]any)["path"].(map[string]any)
	gotFormatOK := got.ResponseFormat.Schema["properties"].(map[string]any)["ok"].(map[string]any)

	if got.Messages[0].ThinkingBlocks[0].Thinking != "step" ||
		got.Messages[0].Calls[0].ID != "call-1" ||
		got.Messages[0].CacheControl.Type != "ephemeral" ||
		got.Tools[0].CacheControl.Type != "ephemeral" ||
		gotPathSchema["type"] != "string" ||
		gotPathSchema["enum"].([]string)[0] != "README.md" ||
		gotFormatOK["type"] != "boolean" ||
		got.CachePrefixMessages != 1 {
		t.Fatalf("captured request mutated with original: %#v", got)
	}
}
