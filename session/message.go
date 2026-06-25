package session

import "strings"

import (
	"encoding/json"
	"time"
)

// Message is the single domain message type, discriminated by role.
// It is the currency of the agent loop and the stored form of the session tree.
// Translated from Pi's pi-ai Message union (UserMessage | AssistantMessage | ToolResultMessage).
//
// The interface is sealed via isMessage; type switches over Message are exhaustive.
type Message interface {
	isMessage()
	timestamp() time.Time
}

// UserMessage is a prompt from the user (or queued input). Content is Text/Image blocks.
// A plain-text user message is constructed with NewUserText.
type UserMessage struct {

	Content   []Content
	Timestamp time.Time
}

// AssistantMessage is a model response. Content is Text/Thinking/ToolCall blocks.
// Failure is encoded in-message: StopReason is error|aborted and Error is non-empty,
// rather than thrown — matching Pi's loop contract.
type AssistantMessage struct {
	Content       []Content // Text | Thinking | ToolCall
	API           string
	Provider      string
	Model         string
	ResponseModel string // optional; set when the provider echoes the actual model
	ResponseID    string // optional
	Usage         Usage
	StopReason    StopReason
	Error         string // non-empty iff StopReason is error or aborted
	Timestamp     time.Time
}

// ToolResultMessage is the result of executing a tool call. It is a peer message
// (role toolResult), not a content block — matching Pi's model.
type ToolResultMessage struct {
	ToolCallID string
	ToolName   string
	Content    []Content // Text | Image
	Details    json.RawMessage
	IsError    bool
	Timestamp  time.Time
}

func (UserMessage) isMessage()       {}
func (AssistantMessage) isMessage()  {}
func (ToolResultMessage) isMessage() {}

func (m UserMessage) timestamp() time.Time       { return m.Timestamp }
func (m AssistantMessage) timestamp() time.Time  { return m.Timestamp }
func (m ToolResultMessage) timestamp() time.Time { return m.Timestamp }

// NewUserText is the string-shorthand constructor for a plain-text user message.
func NewUserText(text string, ts time.Time) *UserMessage {
	return &UserMessage{Content: []Content{TextContent{Text: text}}, Timestamp: ts}
}

// Content is a sealed union of content block kinds.
type Content interface {
	isContent()
}

// TextContent is a block of text (assistant output, user input, or tool result text).
type TextContent struct {
	Text string
}

// ThinkingContent is model reasoning. Signature/Redacted support provider round-tripping
// of redacted thinking (e.g. Gemini).
type ThinkingContent struct {
	Text      string
	Signature string
	Redacted  bool
}

// ImageContent is an image block (user input or tool output).
type ImageContent struct {
	Data     []byte
	MimeType string
}

// ToolCall is a tool invocation requested by the assistant. Arguments are the parsed
// JSON arguments (Pi stores Record<string,any>; map[string]any is the Go equivalent).
type ToolCall struct {
	ID        string
	Name      string
	Arguments map[string]any
	Signature string // optional; provider thinking signature
}

func (TextContent) isContent()     {}
func (ThinkingContent) isContent() {}
func (ImageContent) isContent()    {}
func (*ToolCall) isContent()       {}

// Usage accumulates token counts and cost for a single assistant message.
type Usage struct {
	Input       int
	Output      int
	CacheRead   int
	CacheWrite  int
	TotalTokens int
	Cost        Cost
}

// Cost is the monetary cost breakdown.
type Cost struct {
	Input      float64
	Output     float64
	CacheRead  float64
	CacheWrite float64
	Total      float64
}

// StopReason is the reason an assistant message stopped generating.
type StopReason string

const (
	StopReasonEndTurn  StopReason = "stop"     // natural end of turn
	StopReasonLength   StopReason = "length"  // hit max output tokens
	StopReasonToolUse  StopReason = "toolUse" // stopped to call a tool
	StopReasonError    StopReason = "error"   // provider/transport error
	StopReasonAborted  StopReason = "aborted" // cancelled via signal
)

// EntryRole returns the role of a message entry.
func EntryRole(e Entry) string {
	me, ok := e.(*MessageEntry)
	if !ok {
		return ""
	}
	switch me.Message.(type) {
	case *UserMessage:
		return "user"
	case *AssistantMessage:
		return "assistant"
	case *ToolResultMessage:
		return "tool_result"
	}
	return ""
}

// EntryText returns the text content of a message entry.
func EntryText(e Entry) string {
	me, ok := e.(*MessageEntry)
	if !ok {
		return ""
	}
	var sb strings.Builder
	switch m := me.Message.(type) {
	case *UserMessage:
		for _, c := range m.Content {
			if tc, ok := c.(TextContent); ok {
				sb.WriteString(tc.Text)
			}
		}
	case *AssistantMessage:
		for _, c := range m.Content {
			if tc, ok := c.(TextContent); ok {
				sb.WriteString(tc.Text)
			}
		}
	case *ToolResultMessage:
		for _, c := range m.Content {
			if tc, ok := c.(TextContent); ok {
				sb.WriteString(tc.Text)
			}
		}
	}
	return sb.String()
}

func (UserMessage) isEvent() {}
func (AssistantMessage) isEvent() {}
