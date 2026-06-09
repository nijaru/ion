package llm

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
