package llm

import (
	"encoding/json"
	"fmt"
)

// ContentBlock is the sealed interface for typed content in a message.
// Implementations: TextBlock, ThinkingBlock, ToolCallBlock.
// This is the Go equivalent of Pi's discriminated union
// (TextContent | ThinkingContent | ToolCall).
type ContentBlock interface {
	contentBlock() // unexported marker — sealed to this package
}

// TextBlock represents visible text content.
type TextBlock struct {
	Text string `json:"text"`
}

func (TextBlock) contentBlock() {}

// ThinkingBlock is defined in message.go and implements ContentBlock.

// ToolCallBlock represents a request from the model to invoke a tool.
type ToolCallBlock struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	Arguments string `json:"arguments"` // JSON string, not parsed
}

func (ToolCallBlock) contentBlock() {}

// blockEnvelope wraps a ContentBlock for JSON serialization with a type discriminator.
type blockEnvelope struct {
	Type string          `json:"type"`
	Data json.RawMessage `json:"data"`
}

// MarshalContentBlock serializes a ContentBlock with a type discriminator.
func MarshalContentBlock(block ContentBlock) (json.RawMessage, error) {
	var blockType string
	var data any
	switch b := block.(type) {
	case TextBlock:
		blockType = "text"
		data = b
	case ThinkingBlock:
		blockType = "thinking"
		data = b
	case ToolCallBlock:
		blockType = "tool_call"
		data = b
	default:
		return nil, fmt.Errorf("unknown ContentBlock type: %T", block)
	}
	raw, err := json.Marshal(data)
	if err != nil {
		return nil, err
	}
	return json.Marshal(blockEnvelope{Type: blockType, Data: raw})
}

// UnmarshalContentBlock deserializes a ContentBlock with a type discriminator.
func UnmarshalContentBlock(raw json.RawMessage) (ContentBlock, error) {
	var env blockEnvelope
	if err := json.Unmarshal(raw, &env); err != nil {
		return nil, err
	}
	switch env.Type {
	case "text":
		var b TextBlock
		return b, json.Unmarshal(env.Data, &b)
	case "thinking":
		var b ThinkingBlock
		return b, json.Unmarshal(env.Data, &b)
	case "tool_call":
		var b ToolCallBlock
		return b, json.Unmarshal(env.Data, &b)
	default:
		return nil, fmt.Errorf("unknown ContentBlock type: %q", env.Type)
	}
}

// ContentBlocks is []ContentBlock with custom JSON marshaling.
// The type discriminator is "text", "thinking", or "tool_call".
type ContentBlocks []ContentBlock

func (b ContentBlocks) MarshalJSON() ([]byte, error) {
	items := make([]json.RawMessage, len(b))
	for i, block := range b {
		raw, err := MarshalContentBlock(block)
		if err != nil {
			return nil, err
		}
		items[i] = raw
	}
	return json.Marshal(items)
}

func (b *ContentBlocks) UnmarshalJSON(data []byte) error {
	var raws []json.RawMessage
	if err := json.Unmarshal(data, &raws); err != nil {
		return err
	}
	result := make(ContentBlocks, len(raws))
	for i, raw := range raws {
		block, err := UnmarshalContentBlock(raw)
		if err != nil {
			return err
		}
		result[i] = block
	}
	*b = result
	return nil
}


// StopReason indicates why the model stopped generating.
type StopReason string

const (
	StopReasonStop    StopReason = "stop"     // natural end
	StopReasonLength  StopReason = "length"   // hit max tokens
	StopReasonToolUse StopReason = "toolUse"  // model wants to call tools
	StopReasonError   StopReason = "error"    // provider error
	StopReasonAborted StopReason = "aborted"  // user cancelled
)

// CostBreakdown itemizes cost by token category.
type CostBreakdown struct {
	Input         float64 `json:"input,omitzero"`
	Output        float64 `json:"output,omitzero"`
	CacheRead     float64 `json:"cache_read,omitzero"`
	CacheCreation float64 `json:"cache_creation,omitzero"`
	Total         float64 `json:"total,omitzero"`
}
