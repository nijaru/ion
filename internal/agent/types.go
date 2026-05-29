// Package agent provides the core agent loop primitive for Ion.
// It mirrors Pi's pi-agent-core package, providing a clean separation
// between the agent loop and the product layer (TUI, commands, etc.)
package agent

import (
	"context"

	"github.com/nijaru/ion/internal/llm"
)

// StreamFn is the function signature for streaming LLM completions.
// It must not throw errors; failures are encoded in the returned stream.
type StreamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error)

// ToolExecutor executes a tool call and returns the result.
type ToolExecutor func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error)

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
	ID       string         `json:"id"`
	Name     string         `json:"name"`
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
	ThinkingLevelOff      ThinkingLevel = "off"
	ThinkingLevelMinimal  ThinkingLevel = "minimal"
	ThinkingLevelLow      ThinkingLevel = "low"
	ThinkingLevelMedium   ThinkingLevel = "medium"
	ThinkingLevelHigh     ThinkingLevel = "high"
	ThinkingLevelXHigh    ThinkingLevel = "xhigh"
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
	// Parallel indicates whether the tool can be executed in parallel with other tools.
	Parallel bool `json:"parallel"`
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

// AgentEvent is an event emitted by the agent loop.
type AgentEvent struct {
	// Type is the event type.
	Type AgentEventType `json:"type"`
	// Data is the event-specific data.
	Data any `json:"data,omitempty"`
}

// AgentEventType identifies the type of an agent event.
type AgentEventType string

const (
	// AgentEventTurnStarted is emitted when a new turn starts.
	AgentEventTurnStarted AgentEventType = "turn_started"
	// AgentEventTurnCompleted is emitted when a turn completes.
	AgentEventTurnCompleted AgentEventType = "turn_completed"
	// AgentEventTextDelta is emitted for each text chunk during streaming.
	AgentEventTextDelta AgentEventType = "text_delta"
	// AgentEventThinkingDelta is emitted for each thinking chunk during streaming.
	AgentEventThinkingDelta AgentEventType = "thinking_delta"
	// AgentEventToolCallStarted is emitted when a tool call starts.
	AgentEventToolCallStarted AgentEventType = "tool_call_started"
	// AgentEventToolCallCompleted is emitted when a tool call completes.
	AgentEventToolCallCompleted AgentEventType = "tool_call_completed"
	// AgentEventError is emitted when an error occurs.
	AgentEventError AgentEventType = "error"
)

// AgentLoopConfig is the configuration for the agent loop.
type AgentLoopConfig struct {
	// Model is the model to use for completions.
	Model llm.Model `json:"model"`
	// ThinkingLevel is the initial thinking level.
	ThinkingLevel ThinkingLevel `json:"thinking_level"`
	// StreamFn is the function to use for streaming completions.
	StreamFn StreamFn `json:"-"`
	// ToolExecutionMode controls how tool calls are executed.
	ToolExecutionMode ToolExecutionMode `json:"tool_execution_mode"`
	// QueueMode controls how queued messages are injected.
	QueueMode QueueMode `json:"queue_mode"`
	// MaxTokens is the maximum number of tokens to generate.
	MaxTokens int `json:"max_tokens"`
	// Temperature is the sampling temperature.
	Temperature float64 `json:"temperature"`
	// ToolExecutor executes tool calls.
	ToolExecutor ToolExecutor `json:"-"`

	// Hooks (optional)

	// ConvertToLlm converts AgentMessages to LLM Messages before each call.
	ConvertToLlm func(messages []AgentMessage) []llm.Message `json:"-"`
	// TransformContext transforms the context before each LLM call.
	TransformContext func(messages []AgentMessage) []AgentMessage `json:"-"`
	// ShouldStopAfterTurn is called after each turn to decide whether to stop.
	ShouldStopAfterTurn func(ctx ShouldStopAfterTurnContext) bool `json:"-"`
	// PrepareNextTurn is called before starting another provider request.
	PrepareNextTurn func(ctx PrepareNextTurnContext) *AgentLoopTurnUpdate `json:"-"`
	// GetSteeringMessages returns steering messages to inject mid-run.
	GetSteeringMessages func() []AgentMessage `json:"-"`
	// GetFollowUpMessages returns follow-up messages after the agent stops.
	GetFollowUpMessages func() []AgentMessage `json:"-"`
	// BeforeToolCall is called before each tool execution.
	BeforeToolCall func(ctx BeforeToolCallContext) BeforeToolCallResult `json:"-"`
	// AfterToolCall is called after each tool execution.
	AfterToolCall func(ctx AfterToolCallContext) AfterToolCallResult `json:"-"`
}
