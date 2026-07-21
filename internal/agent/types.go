// Package agent implements the stateless turn engine (RunLoop) and the
// stateful Controller that owns active-session lifecycle. The loop is a pure
// function of its arguments — no persistence, no tree, no retry. Events are
// the sole output.
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
	// SessionID is the stable session identity for provider-side caching/routing.
	// It is distinct from the changing session-tree leaf ID.
	SessionID string
	// TurnID scopes action invocation identity to one durable logical turn.
	TurnID string
	Tools  []Tool

	// ActionBoundary is the runtime-owned effect gate. When configured, every
	// tool marked RequiresAction must pass durable prepare/authorize/start and
	// terminal recording before and after its Execute function.
	ActionBoundary ActionBoundary

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

	// MaxParallelTools bounds the number of tool executors active for one
	// assistant tool batch. Zero uses the runtime default.
	MaxParallelTools int

	// DrainSteer returns queued steering messages (injected before next assistant response).
	DrainSteer func() []session.Message

	// DrainFollowUp returns queued follow-up messages (injected after agent would stop).
	DrainFollowUp func() []session.Message

	// Auth returns the API key and headers for a provider.
	Auth func(model llm.Model) (apiKey string, headers map[string]string)
}

// Tool describes a tool the loop can execute.
type Tool struct {
	Name        string
	Description string
	Parameters  any // JSON Schema for the tool's arguments
	// ReadOnly separates tool availability from the permission/effect boundary.
	// A tool marked read-only does not create an external action record.
	ReadOnly bool
	// RequiresAction marks a tool as capable of an external effect. Production
	// composition sets this explicitly; an action boundary rejects a marked
	// tool that has no logical approval descriptor.
	RequiresAction bool
	// ApprovalRequirement classifies a prepared argument payload. A nil
	// function means the tool is trusted without an interactive decision.
	ApprovalRequirement func(json.RawMessage) (ApprovalRequirement, bool, error)
	Execute             func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error)
	ExecutionMode       ExecMode
	PrepareArgs         func(json.RawMessage) json.RawMessage
}

// ActionRequest is the normalized loop-to-runtime action handoff. The loop
// owns schema validation; the runtime boundary owns canonical identity,
// policy, durability, and the effect boundary.
type ActionRequest struct {
	ToolName     string
	InvocationID string
	SessionID    string
	TurnID       string
	Arguments    json.RawMessage
	Requirement  ApprovalRequirement
	Required     bool
	CWD          string
}

// ActionToken is opaque execution authority returned only after the exact
// action has been durably prepared and authorized.
type ActionToken struct {
	ID     string
	Record session.ActionRecord
}

// ActionResult describes what the executor observed after crossing the start
// boundary. A started action whose outcome cannot be durably finalized remains
// recoverable as indeterminate in the journal.
type ActionResult struct {
	State          session.ActionState
	ResultIdentity string
	Error          string
	CleanupOutcome string
}

// ActionInvoker is the only callback the boundary may use to invoke a tool.
// The loop supplies the already validated tool implementation; the boundary
// owns when that callback is allowed to cross the effect boundary.
type ActionInvoker func(
	ctx context.Context,
	signal <-chan struct{},
	progress func(session.ToolPartial),
) (session.ToolResultMessage, error)

// ActionBoundary is the sole runtime-owned boundary around external effects.
// Implementations must not execute the tool from PrepareAndAuthorize or Start.
type ActionBoundary interface {
	PrepareAndAuthorize(ctx context.Context, request ActionRequest) (*ActionToken, error)
	Execute(ctx context.Context, token *ActionToken, invoke ActionInvoker, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error)
	Cancel(ctx context.Context, token *ActionToken, reason string) error
}

// ApprovalRequirement describes the user-visible scope of a tool operation.
// Tool implementations own classification; the harness owns the decision.
type ApprovalRequirement struct {
	Category      string
	Operation     string
	Resource      string
	Paths         []string
	Environment   []string
	NetworkIntent string
	MCPIdentity   string
	Metadata      map[string]any
	AlwaysConfirm bool
}

// ExecMode controls how multiple tool calls in one assistant message are executed.
type ExecMode string

const (
	ExecParallel   ExecMode = "parallel"
	ExecSequential ExecMode = "sequential"
)

// ToolCallContext is passed to BeforeToolCall.
type ToolCallContext struct {
	RunContext       context.Context
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
	// Error carries an AfterToolCall hook failure back to the loop. The loop
	// turns it into an error ToolResultMessage instead of silently discarding it.
	Error     error
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
	Tools       []Tool
	ToolResults []session.ToolResultMessage // persisted before BuildContext, not in snapshot.Context
}

// StopContext is passed to ShouldStop.
type StopContext struct {
	Message     session.AssistantMessage
	ToolResults []session.ToolResultMessage
	Context     TurnContext
}

// TurnOutcome identifies the durable terminal state of a logical turn.
type TurnOutcome string

const (
	TurnCommitted     TurnOutcome = "committed"
	TurnAborted       TurnOutcome = "aborted"
	TurnFailed        TurnOutcome = "failed"
	TurnIndeterminate TurnOutcome = "indeterminate"
)

// TurnError is returned when a turn produced a terminal assistant message but
// did not complete successfully. The assistant message remains available to
// TurnError now lives in state.go with the phase machine.
// It carries Phase, Kind, RecoveryAction, and Cause.

// Runtime is the narrow turn and event surface consumed by the TUI and CLI.
//
// Session administration is deliberately not part of this interface. Those
// operations are optional capabilities owned by the runtime controller and
// requested by the command that needs them. Keeping this surface small makes
// the turn boundary explicit and prevents the UI from depending on the
// controller's entire implementation.
type Runtime interface {
	// Subscribe opens an independent bounded event stream and returns an
	// authoritative renderable snapshot. A non-zero cursor is used for
	// resubscription; if the stream has advanced, the snapshot is marked
	// Resynced and the caller replaces its projection before reading events.
	Subscribe(ctx context.Context, after EventCursor) (*EventSubscription, error)

	// Prompt submits a user message and runs a full agent turn.
	// Blocks until the turn completes. Returns the final assistant message.
	Prompt(ctx context.Context, text string, images ...session.ImageContent) (session.Message, error)

	// Steer queues a steering message (mid-turn direction change).
	// Returns an error if the harness is idle.
	Steer(text string, images ...session.ImageContent) error

	// FollowUp queues a follow-up message for the next turn.
	// Returns an error if the harness is idle.
	FollowUp(text string, images ...session.ImageContent) error

	// NextTurn queues a message to start a new turn. It rejects closed or full
	// queues rather than silently losing user input.
	NextTurn(text string, images ...session.ImageContent) error

	// Abort cancels the current turn and clears steering/follow-up queues.
	// It returns after the cancellation request is accepted; the runtime remains
	// busy until terminal lifecycle persistence and settlement complete. Returns
	// the cleared messages.
	Abort() ([]session.Message, []session.Message, error)

	// SetModel switches the active model. It rejects a closed runtime.
	SetModel(model llm.Model) error

	// SetThinking changes the thinking level. Idle changes are durable before
	// the live value changes; active changes are applied at the next boundary.
	SetThinking(ctx context.Context, level session.ThinkingLevel) error

	// SetTools updates the complete tool registry and active tool set. Active
	// names must exist in the replacement registry.
	SetTools(tools []Tool, active []string) error

	// ActivateTools adds registered tools to the active set for the next turn.
	// Unknown names fail closed and do not mutate the harness.
	ActivateTools(ctx context.Context, names []string) error

	// Close releases resources.
	Close() error
}

// ActionRecovery is an optional runtime capability for presenting and
// reconciling externally observable actions whose outcome was not proven.
// It is separate from Runtime's turn surface so the TUI/CLI can opt into
// recovery without making ordinary prompt consumers depend on administration.
type ActionRecovery interface {
	UnsettledActions(ctx context.Context) ([]session.ActionRecord, error)
	ReconcileAction(ctx context.Context, actionID string, state session.ActionState, verification, resultIdentity, reason, cleanup string) (session.ActionRecord, error)
}

// ResourceOwner exposes host-created runtime resources that must be closed
// after the controller has stopped. Runtime.Close only terminates controller
// activity; the composition root owns final resource closure.
type ResourceOwner interface {
	CloseResources() error
}

// SessionOwner exposes the current session for read-only projections. It is an
// optional capability rather than part of Runtime because the runtime host is
// the eventual owner of session lifetime.
type SessionOwner interface {
	Session() session.Session
}

// EntryPersister persists a non-turn entry through the runtime controller.
type EntryPersister interface {
	PersistEntry(ctx context.Context, entry session.Entry) error
}

// SessionNamer updates the display metadata for the active session.
type SessionNamer interface {
	AppendSessionInfo(ctx context.Context, name string) (string, error)
}

// SessionForker creates a new session rooted at a source session.
type SessionForker interface {
	ForkSession(ctx context.Context, sourceID string) (string, error)
}

// SessionNavigator moves the active session leaf, optionally preserving the
// abandoned branch with a summary entry.
type SessionNavigator interface {
	NavigateTree(ctx context.Context, targetID string, opts NavigateOptions) (NavigateResult, error)
}

// SessionLabels reads and writes branch labels.
type SessionLabels interface {
	AppendLabel(ctx context.Context, targetID, label string) (string, error)
	GetLabel(ctx context.Context, targetID string) (string, error)
}

// Compactor requests a context compaction at a safe runtime boundary.
type Compactor interface {
	Compact(ctx context.Context) error
}

// NavigateOptions controls optional context preservation when moving to another
// point in the session tree.
type NavigateOptions struct {
	Summarize          bool
	CustomInstructions string
}

// NavigateResult reports the durable entry created by branch summarization.
type NavigateResult struct {
	SummaryEntryID string
}
