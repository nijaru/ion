package anthropic

import (
	"testing"

	sdk "github.com/anthropics/anthropic-sdk-go"
	"github.com/go-json-experiment/json/jsontext"
	"github.com/nijaru/ion/internal/llm"
)

// provider returns a zero-value Provider sufficient for testing convertSchema,
// which does not use the client or config fields.
func provider() *Provider { return &Provider{} }

func TestConvertSchema_Nil(t *testing.T) {
	schema := provider().convertSchema(nil)
	if schema.Properties != nil {
		t.Errorf("nil params: Properties must be nil, got %v", schema.Properties)
	}
	if len(schema.Required) != 0 {
		t.Errorf("nil params: Required must be empty, got %v", schema.Required)
	}
}

func TestConvertSchema_MapWithProperties(t *testing.T) {
	params := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"query": map[string]any{"type": "string"},
		},
		"required": []any{"query"},
	}
	schema := provider().convertSchema(params)

	if schema.Properties == nil {
		t.Fatal("Properties must not be nil")
	}
	if len(schema.Required) != 1 || schema.Required[0] != "query" {
		t.Errorf("Required: got %v, want [query]", schema.Required)
	}
}

func TestConvertSchema_MapWithoutProperties(t *testing.T) {
	// When the schema has no "properties" key, the whole map becomes Properties.
	params := map[string]any{
		"query": map[string]any{"type": "string"},
	}
	schema := provider().convertSchema(params)
	if schema.Properties == nil {
		t.Fatal("Properties must not be nil when schema is a flat map")
	}
}

func TestConvertSchema_JSONRawMessage(t *testing.T) {
	raw := jsontext.Value(`{
		"type": "object",
		"properties": {"path": {"type": "string"}},
		"required": ["path"]
	}`)
	schema := provider().convertSchema(raw)
	if schema.Properties == nil {
		t.Fatal("Properties must not be nil for jsontext.Value input")
	}
	if len(schema.Required) != 1 || schema.Required[0] != "path" {
		t.Errorf("Required: got %v, want [path]", schema.Required)
	}
}

func TestConvertSchema_TypedStruct(t *testing.T) {
	type Params struct {
		Type       string         `json:"type"`
		Properties map[string]any `json:"properties"`
		Required   []string       `json:"required"`
	}
	params := Params{
		Type:       "object",
		Properties: map[string]any{"n": map[string]any{"type": "integer"}},
		Required:   []string{"n"},
	}
	schema := provider().convertSchema(params)
	if schema.Properties == nil {
		t.Fatal("Properties must not be nil for typed struct input")
	}
	if len(schema.Required) != 1 || schema.Required[0] != "n" {
		t.Errorf("Required: got %v, want [n]", schema.Required)
	}
}

func TestConvertSchema_RequiredStrings(t *testing.T) {
	// Verify all string items in a required array are collected.
	params := map[string]any{
		"properties": map[string]any{},
		"required":   []any{"a", "b", 42, "c"}, // 42 is non-string, must be skipped
	}
	schema := provider().convertSchema(params)
	if len(schema.Required) != 3 {
		t.Errorf("Required: got %v, want [a b c]", schema.Required)
	}
}

func TestConvertSchema_NonObjectParams(t *testing.T) {
	// A non-object JSON value (e.g., a string) must not panic and returns empty schema.
	schema := provider().convertSchema("not-an-object")
	if schema.Properties != nil {
		t.Errorf("non-object: Properties must be nil, got %v", schema.Properties)
	}
}

func TestConvertRequestPreservesImageParts(t *testing.T) {
	req := &llm.Request{
		Model: "claude-test",
		Messages: []llm.Message{{
			Role: llm.RoleTool,
			Parts: []llm.ContentPart{
				llm.TextPart("Read image file [image/png]"),
				llm.ImagePart("image/png", "aW1hZ2U="),
			},
			ToolID: "call-1",
			Name:   "read",
		}},
	}

	params := provider().convertRequest(req)
	if len(params.Messages) != 1 {
		t.Fatalf("messages = %d, want 1", len(params.Messages))
	}
	msg := params.Messages[0]
	if len(msg.Content) != 1 || msg.Content[0].OfToolResult == nil {
		t.Fatalf("message content = %+v, want tool result", msg.Content)
	}
	toolResult := msg.Content[0].OfToolResult
	if toolResult.ToolUseID != "call-1" {
		t.Fatalf("tool use id = %q, want call-1", toolResult.ToolUseID)
	}
	if len(toolResult.Content) != 2 {
		t.Fatalf("tool result content = %+v, want text and image", toolResult.Content)
	}
	if got := toolResult.Content[0].OfText.Text; got != "Read image file [image/png]" {
		t.Fatalf("text part = %q", got)
	}
	image := toolResult.Content[1].OfImage
	if image == nil ||
		image.Source.OfBase64 == nil ||
		image.Source.OfBase64.MediaType != sdk.Base64ImageSourceMediaTypeImagePNG ||
		image.Source.OfBase64.Data != "aW1hZ2U=" {
		t.Fatalf("image part = %+v", toolResult.Content[1])
	}
}

func TestIsContextOverflowMessage(t *testing.T) {
	if !isContextOverflowMessage("Prompt is too long: max TOKENS exceeded") {
		t.Fatal("expected mixed-case prompt/token message to match")
	}
	if isContextOverflowMessage("rate limit exceeded") {
		t.Fatal("expected unrelated message not to match")
	}
}

func TestUsageFromMessageIncludesCacheTokens(t *testing.T) {
	got := usageFromMessage(sdk.Usage{
		InputTokens:              10,
		OutputTokens:             5,
		CacheReadInputTokens:     3,
		CacheCreationInputTokens: 2,
	})

	if got.InputTokens != 10 || got.OutputTokens != 5 ||
		got.CacheReadTokens != 3 || got.CacheCreationTokens != 2 {
		t.Fatalf("usage = %+v, want input/output/cache fields copied", got)
	}
}

func TestStreamUsageChunksAreCumulative(t *testing.T) {
	p := &Provider{
		config: llmProviderConfigForTest(),
	}
	stream := &Stream{p: p, model: "test-model"}

	first := stream.updateUsage(usageFromMessage(sdk.Usage{
		InputTokens:              10,
		CacheReadInputTokens:     2,
		CacheCreationInputTokens: 3,
	}))
	second := stream.updateUsage(usageFromMessageDelta(sdk.MessageDeltaUsage{
		InputTokens:              10,
		OutputTokens:             5,
		CacheReadInputTokens:     2,
		CacheCreationInputTokens: 3,
	}))

	if first.Usage.TotalTokens != 10 {
		t.Fatalf("first total = %d, want 10", first.Usage.TotalTokens)
	}
	if second.Usage.TotalTokens != 15 {
		t.Fatalf("second total = %d, want cumulative 15", second.Usage.TotalTokens)
	}
	if second.Usage.Cost == 0 {
		t.Fatal("expected stream usage cost to include cumulative input/output usage")
	}
}

func llmProviderConfigForTest() llm.ProviderConfig {
	return llm.ProviderConfig{
		Models: []llm.Model{{
			ID:           "test-model",
			CostPer1MIn:  1,
			CostPer1MOut: 2,
		}},
	}
}
