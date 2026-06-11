// Package agent provides the core agent loop primitive for Ion.
// It mirrors Pi's pi-agent-core package, providing a clean separation
// between the agent loop and the product layer (TUI, commands, etc.)
package agent

import (
	"context"

	"github.com/nijaru/ion/llm"
)

// StreamFn is the function signature for streaming LLM completions.
// It must not throw errors; failures are encoded in the returned stream.
type StreamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error)

// ToolExecutor executes a tool call and returns the result.
type ToolExecutor func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error)

// ModelMessageWriter persists one provider-visible message.
type ModelMessageWriter func(ctx context.Context, message llm.Message) error

// ToolExecutionMode controls how tool calls from a single assistant message are executed.
type ToolExecutionMode string

const (
	// ToolExecutionSequential executes each tool call one by one.
	ToolExecutionSequential ToolExecutionMode = "sequential"
	// ToolExecutionParallel executes allowed tools concurrently.
	ToolExecutionParallel ToolExecutionMode = "parallel"
)

// QueueMode controls how queued user messages are injected when the agent loop reaches a drain point.
type QueueMode string

const (
	// QueueModeAll drains and injects every queued message at that point.
	QueueModeAll QueueMode = "all"
	// QueueModeOneAtATime drains and injects only the oldest queued message.
	QueueModeOneAtATime QueueMode = "one-at-a-time"
)

// AgentToolCall represents a single tool call from an assistant message.
type AgentToolCall struct {
	ID        string         `json:"id"`
	Name      string         `json:"name"`
	Arguments map[string]any `json:"arguments"`
}

// BeforeToolCallResult is returned from the beforeToolCall hook.
// Returning Block=true prevents the tool from executing.
type BeforeToolCallResult struct {
	Block  bool   `json:"block,omitempty"`
	Reason string `json:"reason,omitempty"`
}

// AfterToolCallResult is returned from the afterToolCall hook.
// Fields are merged field-by-field with the original result.
type AfterToolCallResult struct {
	Content   []llm.ContentPart `json:"content,omitempty"`
	Details   any               `json:"details,omitempty"`
	IsError   *bool             `json:"isError,omitempty"`
	Terminate *bool             `json:"terminate,omitempty"`
}

// BeforeToolCallContext is passed to the beforeToolCall hook.
type BeforeToolCallContext struct {
	// AssistantMessage is the assistant message that requested the tool call.
	AssistantMessage llm.Message
	// ToolCall is the raw tool call from the assistant message.
	ToolCall AgentToolCall
	// Args is the validated tool arguments for the target tool schema.
	Args any
	// Context is the current agent context at the time the tool call is prepared.
	Context AgentContext
}

// AfterToolCallContext is passed to the afterToolCall hook.
type AfterToolCallContext struct {
	// AssistantMessage is the assistant message that requested the tool call.
	AssistantMessage llm.Message
	// ToolCall is the raw tool call from the assistant message.
	ToolCall AgentToolCall
	// Args is the validated tool arguments for the target tool schema.
	Args any
	// Result is the executed tool result before any overrides are applied.
	Result AgentToolResult
	// IsError indicates whether the executed tool result is currently treated as an error.
	IsError bool
	// Context is the current agent context at the time the tool call is finalized.
	Context AgentContext
}

// ShouldStopAfterTurnContext is passed to the shouldStopAfterTurn hook.
type ShouldStopAfterTurnContext struct {
	// Message is the assistant message that completed the turn.
	Message llm.Message
	// ToolResults are the tool result messages from the turn.
	ToolResults []llm.Message
	// Context is the current agent context after the turn.
	Context AgentContext
	// NewMessages are the messages added during this loop invocation.
	NewMessages []AgentMessage
}

// AgentLoopTurnUpdate is used to replace runtime state before starting another provider request.
type AgentLoopTurnUpdate struct {
	// Context for the next provider request.
	Context *AgentContext
	// Model for the next provider request.
	Model *llm.Model
	// ThinkingLevel for the next provider request.
	ThinkingLevel *ThinkingLevel
}

// PrepareNextTurnContext extends ShouldStopAfterTurnContext with additional context.
type PrepareNextTurnContext = ShouldStopAfterTurnContext

// ThinkingLevel controls the depth of internal reasoning.
type ThinkingLevel string

const (
	ThinkingLevelOff     ThinkingLevel = "off"
	ThinkingLevelMinimal ThinkingLevel = "minimal"
	ThinkingLevelLow     ThinkingLevel = "low"
	ThinkingLevelMedium  ThinkingLevel = "medium"
	ThinkingLevelHigh    ThinkingLevel = "high"
	ThinkingLevelXHigh   ThinkingLevel = "xhigh"
)

// AgentMessage is a message in the agent's conversation.
// It can be a standard LLM message or a custom message type.
type AgentMessage struct {
	// Role of the message (user, assistant, tool, system).
	Role string `json:"role"`
	// Content is the text content of the message.
	Content string `json:"content"`
	// Parts are structured content parts (text, images, etc.).
	Parts []llm.ContentPart `json:"parts,omitempty"`
	// Reasoning is the model's internal reasoning (for reasoning models).
	Reasoning string `json:"reasoning,omitempty"`
	// Calls are tool calls made by the assistant.
	Calls []AgentToolCall `json:"calls,omitempty"`
	// ToolID is the ID of the tool call this message is a result for.
	ToolID string `json:"tool_id,omitempty"`
	// Name is the name of the tool or assistant.
	Name string `json:"name,omitempty"`
	// IsError indicates whether this is an error result.
	IsError bool `json:"is_error,omitempty"`
	// Usage is the LLM's reported token usage for this message.
	InputTokens  int     `json:"input_tokens,omitzero"`
	OutputTokens int     `json:"output_tokens,omitzero"`
	TotalTokens  int     `json:"total_tokens,omitzero"`
	Cost         float64 `json:"cost,omitzero"`
	// Timestamp is when the message was created.
	Timestamp int64 `json:"timestamp,omitempty"`
}

// AgentToolResult is the result of executing a tool.
type AgentToolResult struct {
	// Content is the text content of the result.
	Content []llm.ContentPart `json:"content"`
	// Details is additional structured data from the tool.
	Details any `json:"details,omitempty"`
	// IsError indicates whether the tool execution failed.
	IsError bool `json:"is_error"`
	// Terminate hints that the agent should stop after this tool batch.
	Terminate bool `json:"terminate,omitempty"`
}

// AgentContext is the context passed to agent hooks.
type AgentContext struct {
	// Messages is the current conversation history.
	Messages []AgentMessage `json:"messages"`
	// SystemPrompt is the system prompt for the agent.
	SystemPrompt string `json:"system_prompt"`
	// Tools is the list of available tools.
	Tools []AgentTool `json:"tools"`
	// Model is the current model being used.
	Model llm.Model `json:"model"`
	// ThinkingLevel is the current thinking level.
	ThinkingLevel ThinkingLevel `json:"thinking_level"`
}

// AgentTool is a tool definition with optional hooks.
type AgentTool struct {
	// Name is the unique identifier for the tool.
	Name string `json:"name"`
	// Description is a human-readable description of the tool.
	Description string `json:"description"`
	// Parameters is the JSON Schema for the tool's parameters.
	Parameters any `json:"parameters"`
	// ReadOnly indicates whether the tool only reads data.
	ReadOnly bool `json:"read_only"`
	// ExecutionMode controls how this tool's calls are executed (sequential or parallel).
	// If empty, uses the global ToolExecutionMode from AgentConfig.
	ExecutionMode ToolExecutionMode `json:"execution_mode,omitempty"`
	// Label is a human-readable label for the tool (shown in TUI).
	Label string `json:"label,omitzero"`
	// PrepareArguments transforms tool arguments before validation and execution.
	// Called after the LLM produces arguments but before validateToolArgs.
	// Use for type coercion, default injection, or argument normalization.
	PrepareArguments func(args map[string]any) map[string]any `json:"-"`
}

// AgentState is the current state of the agent.
type AgentState struct {
	// Messages is the current conversation history.
	Messages []AgentMessage `json:"messages"`
	// Model is the current model being used.
	Model llm.Model `json:"model"`
	// ThinkingLevel is the current thinking level.
	ThinkingLevel ThinkingLevel `json:"thinking_level"`
	// Tools is the list of available tools.
	Tools []AgentTool `json:"tools"`
	// SystemPrompt is the system prompt for the agent.
	SystemPrompt string `json:"system_prompt"`
	// IsStreaming indicates whether the agent is currently streaming.
	IsStreaming bool `json:"is_streaming"`
	// ErrorMessage is the last error message, if any.
	ErrorMessage string `json:"error_message,omitempty"`
}


