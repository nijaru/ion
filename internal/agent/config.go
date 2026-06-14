package agent

import (
	"context"
	"os"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// AgentConfig is the configuration for an Agent instance.
// It defines the agent's behavior, callbacks, and hooks.
type AgentConfig struct {
	// ID is the session identifier.
	ID string `json:"id,omitempty"`

	// Core settings
	Model         llm.Model       `json:"model"`
	ThinkingLevel ThinkingLevel   `json:"thinking_level"`
	SystemPrompt  string          `json:"system_prompt,omitempty"`
	Tools         []AgentTool     `json:"tools,omitempty"`
	StreamFn      StreamFn        `json:"-"`
	ToolExecutor  ToolExecutor    `json:"-"`
	// StreamingToolExecutor is an optional tool executor that supports progress updates.
	// If set, it is used instead of ToolExecutor for tools that support streaming.
	StreamingToolExecutor StreamingToolExecutor `json:"-"`
	OnEvent       func(event session.AgentEvent) `json:"-"`
	OnModelMessage ModelMessageWriter `json:"-"`

	// Execution settings
	ToolExecutionMode ToolExecutionMode `json:"tool_execution_mode"`
	QueueMode         QueueMode         `json:"queue_mode"`
	MaxTokens         int               `json:"max_tokens"`
	Temperature       float64           `json:"temperature"`

	// Retry settings
	// MaxRetries is the max number of retry attempts for transient errors.
	// Default: 3
	MaxRetries int `json:"max_retries,omitempty"`
	// RetryBaseDelayMs is the base delay in ms for exponential backoff.
	// Default: 1000
	RetryBaseDelayMs int `json:"retry_base_delay_ms,omitempty"`

	// Callbacks (Pi parity)
	// ConvertToLlm converts AgentMessages to LLM Messages before each call.
	// Default: filter to standard roles (user, assistant, tool, system).
	ConvertToLlm func(messages []AgentMessage) []llm.Message `json:"-"`
	// TransformContext transforms the message context before each LLM call.
	// Default: no-op (returns messages unchanged).
	TransformContext func(ctx context.Context, messages []AgentMessage) []AgentMessage `json:"-"`
	// GetApiKey returns the API key for a given provider.
	// Default: os.Getenv(provider + "_API_KEY").
	GetApiKey func(provider string) string `json:"-"`
	// ShouldStopAfterTurn is called after each turn to decide whether to stop.
	// Default: nil (never stops early).
	ShouldStopAfterTurn func(ctx ShouldStopAfterTurnContext) bool `json:"-"`
	// PrepareNextTurn is called before starting another provider request.
	// Default: nil (no state changes).
	PrepareNextTurn func(ctx PrepareNextTurnContext) *AgentLoopTurnUpdate `json:"-"`
	// HandleRunFailure is called when the agent run fails.
	// Default: nil (no-op).
	HandleRunFailure func(err error) `json:"-"`
	// GetSteeringMessages returns steering messages to inject mid-run.
	// Default: nil (no steering).
	GetSteeringMessages func() []AgentMessage `json:"-"`
	// GetFollowUpMessages returns follow-up messages after the agent stops.
	// Default: nil (no follow-up).
	GetFollowUpMessages func() []AgentMessage `json:"-"`

	// Compaction
	// CompactFunc is the function to call for compaction.
	// If nil, compaction is skipped.
	CompactFunc func(ctx context.Context) (bool, error) `json:"-"`

	// Tool hooks
	// BeforeToolCall is called before each tool execution.
	// Default: nil (no blocking).
	BeforeToolCall func(ctx context.Context, hookCtx BeforeToolCallContext) BeforeToolCallResult `json:"-"`
	// AfterToolCall is called after each tool execution.
	// Default: nil (no modifications).
	AfterToolCall func(ctx context.Context, hookCtx AfterToolCallContext) AfterToolCallResult `json:"-"`
	// OnToolProgress is called during tool execution with partial results.
	// This enables streaming tool output to the TUI in real-time.
	// Default: nil (no streaming).
	OnToolProgress func(ctx context.Context, toolUseID, toolName string, partialResult any) `json:"-"`
}

// DefaultConvertToLlm filters AgentMessages to standard LLM roles.
func DefaultConvertToLlm(messages []AgentMessage) []llm.Message {
	var result []llm.Message
	for _, msg := range messages {
		switch msg.Role {
		case "user", "assistant", "tool", "system":
			result = append(result, agentMessageToLLM(msg))
		}
	}
	return result
}

// DefaultGetApiKey returns the API key from environment variables.
func DefaultGetApiKey(provider string) string {
	return os.Getenv(provider + "_API_KEY")
}

// WithDefaults returns a copy of the config with nil callbacks filled in with defaults.
func (c AgentConfig) WithDefaults() AgentConfig {
	if c.ConvertToLlm == nil {
		c.ConvertToLlm = DefaultConvertToLlm
	}
	if c.TransformContext == nil {
		c.TransformContext = func(ctx context.Context, messages []AgentMessage) []AgentMessage {
			return messages
		}
	}
	if c.GetApiKey == nil {
		c.GetApiKey = DefaultGetApiKey
	}
	return c
}

// GetMaxRetries returns the max retry attempts (default 3).
func (c *AgentConfig) GetMaxRetries() int {
	if c == nil || c.MaxRetries <= 0 {
		return defaultMaxRetries
	}
	if c.MaxRetries > 10 {
		return 10
	}
	return c.MaxRetries
}

// GetRetryBaseDelayMs returns the base delay in ms for exponential backoff (default 1000).
func (c *AgentConfig) GetRetryBaseDelayMs() int {
	if c == nil || c.RetryBaseDelayMs <= 0 {
		return defaultRetryBaseDelayMs
	}
	if c.RetryBaseDelayMs > 60000 {
		return 60000
	}
	return c.RetryBaseDelayMs
}

const (
	defaultMaxRetries       = 3
	defaultRetryBaseDelayMs = 1000
)
