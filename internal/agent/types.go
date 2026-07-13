// Package agent implements the stateless turn engine (RunLoop) and will hold
// the stateful harness in a later phase. The loop is a pure function of its
// arguments — no persistence, no tree, no retry. Events are the sole output.
//
// Reference: Pi's agent-loop.js (509 lines).
package agent

import (
	"context"
	"encoding/json"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// RunLoop is defined in loop.go.
// This file contains only the types used by the loop and harness.

// TurnContext is the snapshot the harness builds from the session each turn.
// The loop operates on this snapshot — it never touches the session directly.
type TurnContext struct {
	SystemPrompt string
	Messages     []session.Message // from session.buildContext()
}

// LoopConfig is the per-turn contract between harness and loop.
// Built fresh by the harness each turn (Pi createLoopConfig).
type LoopConfig struct {
	Model    llm.Model
	Thinking session.ThinkingLevel
	Tools    []Tool

	// StreamFn calls the LLM provider. The loop constructs an llm.Request
	// and passes it here. The harness wraps this with auth/hooks.
	StreamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error)

	// Convert transforms domain Messages to provider Messages at the boundary.
	// Default: filter to user/assistant/tool_result roles.
	Convert func([]session.Message) []llm.Message

	// TransformCtx optionally transforms the message context before each LLM call.
	TransformCtx func(ctx context.Context, msgs []session.Message) []session.Message

	// BeforeToolCall is called before executing a tool. Return non-nil to block.
	BeforeToolCall func(ctx ToolCallContext) *ToolCallDecision

	// AfterToolCall is called after executing a tool. Return non-nil to patch.
	AfterToolCall func(ctx ToolCallResultContext) *ToolCallPatch

	// PrepareNextTurn is called between turns. The harness uses this to flush
	// buffered session writes, persist tool results, and rebuild the context before
	// the next LLM call. The loop passes tool results so they can be persisted
	// synchronously before BuildContext reads them.
	PrepareNextTurn func(ctx context.Context, toolResults []session.ToolResultMessage) *NextTurnSnapshot

	// ShouldStop is called after each turn to decide whether to stop.
	ShouldStop func(ctx StopContext) bool

	// MaxToolIterations is a safety cap on the inner tool execution loop.
	// Prevents infinite loops if the LLM keeps requesting tools without terminating.
	// Default: 25.
	MaxToolIterations int

	// DrainSteer returns queued steering messages (injected before next assistant response).
	DrainSteer func() []session.Message

	// DrainFollowUp returns queued follow-up messages (injected after agent would stop).
	DrainFollowUp func() []session.Message

	// Auth returns the API key and headers for a provider.
	Auth func(model llm.Model) (apiKey string, headers map[string]string)
}

// Tool describes a tool the loop can execute.
type Tool struct {
	Name          string
	Description   string
	Parameters    any // JSON Schema for the tool's arguments
	Execute       func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error)
	ExecutionMode ExecMode
	PrepareArgs   func(json.RawMessage) json.RawMessage
}

// ExecMode controls how multiple tool calls in one assistant message are executed.
type ExecMode string

const (
	ExecParallel   ExecMode = "parallel"
	ExecSequential ExecMode = "sequential"
)

// ToolCallContext is passed to BeforeToolCall.
type ToolCallContext struct {
	AssistantMessage session.AssistantMessage
	ToolCall         *session.ToolCall
	Args             json.RawMessage
	Context          TurnContext
}

// ToolCallDecision is returned by BeforeToolCall to block execution.
type ToolCallDecision struct {
	Block  bool
	Reason string
}

// ToolCallResultContext is passed to AfterToolCall.
type ToolCallResultContext struct {
	ToolCall *session.ToolCall
	Args     json.RawMessage
	Result   session.ToolResultMessage
}

// ToolCallPatch is returned by AfterToolCall to modify the result.
type ToolCallPatch struct {
	Content   []session.Content
	Details   json.RawMessage
	IsError   *bool
	Terminate *bool
}

// NextTurnSnapshot is returned by PrepareNextTurn to rebuild the context.
type NextTurnSnapshot struct {
	Context     TurnContext
	Model       *llm.Model
	Thinking    *session.ThinkingLevel
	ToolResults []session.ToolResultMessage // persisted before BuildContext, not in snapshot.Context
}

// StopContext is passed to ShouldStop.
type StopContext struct {
	Message     session.AssistantMessage
	ToolResults []session.ToolResultMessage
	Context     TurnContext
}

// Runner is the interface app/ uses to drive the agent.
// The Harness implements this. It replaces the old Backend + Session pattern.
type Runner interface {
	// Events returns the channel the TUI subscribes to for agent events.
	Events() <-chan session.Event

	// Prompt submits a user message and runs a full agent turn.
	// Blocks until the turn completes. Returns the final assistant message.
	Prompt(ctx context.Context, text string) (session.Message, error)

	// Steer queues a steering message (mid-turn direction change).
	// Returns an error if the harness is idle.
	Steer(text string) error

	// FollowUp queues a follow-up message for the next turn.
	// Returns an error if the harness is idle.
	FollowUp(text string) error

	// NextTurn queues a message to start a new turn.
	NextTurn(text string)

	// Abort cancels the current turn and clears steering/follow-up queues.
	// Returns the cleared messages.
	Abort() ([]session.Message, []session.Message, error)

	// SetModel switches the active model.
	SetModel(model llm.Model)

	// SetThinking changes the thinking level.
	SetThinking(level session.ThinkingLevel)

	// SetTools updates the active tool set.
	SetTools(tools []Tool, active []string)

	// Session returns the underlying session for auxiliary reads.
	Session() session.Session

	// Append persists an auxiliary entry through the harness-owned session.
	PersistEntry(ctx context.Context, entry session.Entry) error
	AppendSessionInfo(ctx context.Context, name string) (string, error)
	MoveTo(ctx context.Context, entryID string, summary *session.BranchSummaryData) (string, error)
	AppendLabel(ctx context.Context, targetID, label string) (string, error)
	GetLabel(ctx context.Context, targetID string) (string, error)

	// Compact triggers context compaction.
	Compact(ctx context.Context) error

	// Close releases resources.
	Close() error
}
