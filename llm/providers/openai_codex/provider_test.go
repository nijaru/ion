package openaicodex

import (
	"bufio"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestProviderStreamsResponsesEvents(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/codex/responses" {
			t.Fatalf("path = %q, want /codex/responses", r.URL.Path)
		}
		if got := r.Header.Get("Authorization"); got != "Bearer access-token" {
			t.Fatalf("authorization = %q", got)
		}
		if got := r.Header.Get("chatgpt-account-id"); got != "acct-test" {
			t.Fatalf("account ID = %q", got)
		}
		var request requestBody
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Fatalf("decode request: %v", err)
		}
		if request.Model != "gpt-5.6-luna" || request.Instructions != "system" {
			t.Fatalf("request = %#v", request)
		}
		if len(request.Tools) != 1 || request.Tools[0].Name != "read" {
			t.Fatalf("tools = %#v", request.Tools)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		writer := bufio.NewWriter(w)
		for _, event := range []string{
			`{"type":"response.created","response":{"id":"resp-1"}}`,
			`{"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant"}}`,
			`{"type":"response.output_text.delta","output_index":0,"delta":"hello"}`,
			`{"type":"response.output_item.done","output_index":0,"item":{"type":"message","content":[{"type":"output_text","text":"hello"}],"phase":"final_answer"}}`,
			`{"type":"response.completed","response":{"id":"resp-1","status":"completed","output":[{"type":"message"}],"usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15}}}`,
		} {
			_, _ = writer.WriteString("data: " + event + "\n\n")
		}
		_ = writer.Flush()
	}))
	defer server.Close()

	provider := NewProvider(llm.ProviderConfig{
		ID:          "openai-codex",
		APIKey:      "access-token",
		AccountID:   "acct-test",
		APIEndpoint: server.URL,
		Models: []llm.Model{{
			ID: "gpt-5.6-luna", CostPer1MIn: 1, CostPer1MOut: 2,
		}},
	})
	request := &llm.Request{
		Model: "gpt-5.6-luna",
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "hello"},
		},
		Tools: []*llm.Spec{{Name: "read", Description: "Read a file", Parameters: map[string]any{"type": "object"}}},
	}
	response, err := provider.Generate(context.Background(), request)
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	if response.TextContent() != "hello" || response.ResponseID != "resp-1" {
		t.Fatalf("response = %#v", response)
	}
	if response.Usage.InputTokens != 12 || response.Usage.OutputTokens != 3 {
		t.Fatalf("usage = %#v", response.Usage)
	}
	if response.StopReason != llm.StopReasonStop {
		t.Fatalf("stop reason = %q, want stop", response.StopReason)
	}
}

func TestProviderRejectsPrematureEOF(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n"))
	}))
	defer server.Close()

	provider := NewProvider(llm.ProviderConfig{
		APIKey: "access-token", AccountID: "acct-test", APIEndpoint: server.URL,
	})
	_, err := provider.Generate(context.Background(), &llm.Request{Model: "gpt-5.6-luna"})
	if err == nil || !strings.Contains(err.Error(), "before a terminal response event") {
		t.Fatalf("error = %v, want premature EOF error", err)
	}
}

func TestProviderStreamsToolCall(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		for _, event := range []string{
			`{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call-1","name":"read","arguments":""}}`,
			`{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"path\":\"a\"}"}`,
			`{"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"call-1","name":"read","arguments":"{\"path\":\"a\"}"}}`,
			`{"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call"}]}}`,
		} {
			_, _ = w.Write([]byte("data: " + event + "\n\n"))
		}
	}))
	defer server.Close()

	provider := NewProvider(llm.ProviderConfig{
		APIKey: "access-token", AccountID: "acct-test", APIEndpoint: server.URL,
	})
	response, err := provider.Generate(context.Background(), &llm.Request{
		Model:    "gpt-5.6-luna",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "read a"}},
	})
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	calls := response.ToolCalls()
	if len(calls) != 1 || calls[0].ID != "call-1" || calls[0].Function.Name != "read" ||
		calls[0].Function.Arguments != `{"path":"a"}` {
		t.Fatalf("calls = %#v", calls)
	}
	if response.StopReason != llm.StopReasonToolUse {
		t.Fatalf("stop reason = %q, want toolUse", response.StopReason)
	}
}

func TestBuildRequestPreservesCodexResponseItemIDs(t *testing.T) {
	request := buildRequest(&llm.Request{
		Model: "gpt-5.6-luna",
		Messages: []llm.Message{
			{
				Role: llm.RoleAssistant,
				Blocks: llm.ContentBlocks{
					llm.ToolCallBlock{ID: "call-1|fc_item", Name: "read", Arguments: `{"path":"a"}`},
				},
			},
			{Role: llm.RoleTool, ToolID: "call-1|fc_item", Content: "ok"},
		},
	})
	data, err := json.Marshal(request)
	if err != nil {
		t.Fatalf("marshal request: %v", err)
	}
	var decoded struct {
		Input []map[string]any `json:"input"`
	}
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("decode request: %v", err)
	}
	if got := decoded.Input[0]["id"]; got != "fc_item" {
		t.Fatalf("function item ID = %v, want fc_item", got)
	}
	if got := decoded.Input[0]["call_id"]; got != "call-1" {
		t.Fatalf("function call ID = %v, want call-1", got)
	}
	if got := decoded.Input[1]["call_id"]; got != "call-1" {
		t.Fatalf("function output call ID = %v, want call-1", got)
	}
}

func TestProviderRejectsHTTPError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "not authorized", http.StatusUnauthorized)
	}))
	defer server.Close()

	provider := NewProvider(llm.ProviderConfig{
		APIKey: "access-token", AccountID: "acct-test", APIEndpoint: server.URL,
	})
	_, err := provider.Stream(context.Background(), &llm.Request{Model: "gpt-5.6-luna"})
	if err == nil || !strings.Contains(err.Error(), "HTTP 401") {
		t.Fatalf("error = %v, want HTTP 401", err)
	}
}
