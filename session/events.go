package session

import "time"

// Event is the closed union of events the agent loop and harness emit. The
// core lifecycle follows a provider-independent agent contract; runtime
// control events extend it for host interaction and persistence projection:
//
//	agent_start, turn_start, message_start, message_update, message_end,
//	tool_execution_start, tool_execution_update, tool_execution_end,
//	turn_end, agent_end
//
// ApprovalRequest, ApprovalResolution, and ProviderRetry are runtime-only
// control events; tool results remain the durable session record.
//
// message_update carries a Delta union (text/thinking/toolcall).
type Event interface {
	isEvent()
}

// --- Core events. ---

// AgentStart opens a run. Origin tags root vs child for the future subagent seam.
type AgentStart struct {
	Origin SessionOrigin
}

// TurnStart opens a turn (one assistant response + its tool execution).
type TurnStart struct {
	Timestamp time.Time
	// TurnToken is runtime-owned cancellation identity. Stateless loop
	// emitters leave it zero; the controller enriches lifecycle events before
	// publication so frontends can fence delayed cancellation commands.
	TurnToken uint64
}

// MessageStart opens a message (user prompt, assistant response, or tool result).
type MessageStart struct {
	Message Message
}

// MessageUpdate carries an incremental update to the streaming assistant message.
type MessageUpdate struct {
	Message   Message
	Delta     Delta
	BlockType string
	AgentID   string
	Timestamp time.Time
}

// MessageEnd closes a message; Message is final.
type MessageEnd struct {
	Message   Message
	Timestamp time.Time
}

// ToolExecStart opens execution of one tool call.
type ToolExecStart struct {
	ToolCallID string
	Name       string
	Args       []byte
}

// ToolExecUpdate carries a progress update from a running tool.
type ToolExecUpdate struct {
	ToolCallID string
	Partial    ToolPartial
}

// ToolExecEnd closes execution of one tool call with its result.
type ToolExecEnd struct {
	ToolCallID string
	Result     ToolResultMessage
}

// ApprovalDecision is the outcome of an interactive tool trust request.
// Decisions are runtime-only control events; the resulting ToolResultMessage
// is what enters the session tree.
type ApprovalDecision string

const (
	ApprovalAllow  ApprovalDecision = "allow"
	ApprovalDeny   ApprovalDecision = "deny"
	ApprovalAlways ApprovalDecision = "always"
)

// ApprovalRequest pauses a requirement-bearing tool call until the host
// resolves it. ID is unique within the active harness runtime.
type ApprovalRequest struct {
	ID          string
	ActionID    string
	Fingerprint string
	ToolCallID  string
	ToolName    string
	Category    string
	Operation   string
	Resource    string
	CWD         string
	Paths       []string
	Timestamp   time.Time
}

// ApprovalResolution closes one ApprovalRequest exactly once.
type ApprovalResolution struct {
	ID        string
	Decision  ApprovalDecision
	Reason    string
	Timestamp time.Time
}

// TurnEnd closes a turn.
type TurnEnd struct {
	Error       error
	Base        EventBase
	Message     Message
	ToolResults []ToolResultMessage
}

// AgentEnd closes a run.
type AgentEnd struct {
	Messages []Message
}

// --- Delta union (streaming deltas). ---

type Delta interface {
	isDelta()
}

type TextDelta struct {
	Text string
}

type ThinkingDelta struct {
	Text string
}

type ToolCallDelta struct {
	ToolCallID     string
	Name           string
	ArgumentsChunk string
}

func (TextDelta) isDelta()     {}
func (ThinkingDelta) isDelta() {}
func (ToolCallDelta) isDelta() {}

// --- Metadata. ---

// SessionOrigin identifies which session an event belongs to.
// The subagent seam — present now, unused until subagents ship.
type SessionOrigin struct {
	SessionID string
	ChildID   string
}

// EventBase carries common event metadata.
type EventBase struct {
	Timestamp time.Time
	Error     error
}

func BaseNow() EventBase { return EventBase{Timestamp: time.Now()} }

// ToolPartial is a progress payload from a running tool (opaque to the loop).
type ToolPartial = any

// --- Sealing. ---

func (AgentStart) isEvent()         {}
func (TurnStart) isEvent()          {}
func (MessageStart) isEvent()       {}
func (MessageUpdate) isEvent()      {}
func (MessageEnd) isEvent()         {}
func (ToolExecStart) isEvent()      {}
func (ToolExecUpdate) isEvent()     {}
func (ToolExecEnd) isEvent()        {}
func (ApprovalRequest) isEvent()    {}
func (ApprovalResolution) isEvent() {}
func (TurnEnd) isEvent()            {}
func (AgentEnd) isEvent()           {}

// --- Controller lifecycle events. ---

// UpdateSource identifies why a harness setting changed.
type UpdateSource string

const UpdateSourceSet UpdateSource = "set"

// ModelUpdate is emitted when the active model changes.
type ModelUpdate struct {
	Model    string
	Previous string
	Source   UpdateSource
}

// ThinkingUpdate is emitted when the active thinking level changes.
type ThinkingUpdate struct {
	Level    ThinkingLevel
	Previous ThinkingLevel
}

// ToolsUpdate is emitted when the available or active tool names change.
type ToolsUpdate struct {
	Active   []string
	Previous []string
}

// QueueUpdate is emitted when steer/followUp/nextTurn queues change.
// Carries full queued messages, not just counts.
type QueueUpdate struct {
	Steer    []Message
	FollowUp []Message
	NextTurn []Message
}

// Settled is emitted after agent_end, signaling the harness is back to idle.
type Settled struct {
	NextTurnCount int
}

// RuntimeReady is emitted after a non-turn exclusive operation returns the
// runtime to ready. It is distinct from Settled because no agent turn ended.
type RuntimeReady struct{}

// SavePoint is emitted after turn_end flush, signaling all writes are durable.
type SavePoint struct {
	HadPendingMutations bool
}

// Abort is emitted when the current run is aborted, carrying cleared queues.
type Abort struct {
	ClearedSteer    []Message
	ClearedFollowUp []Message
}

func (ModelUpdate) isEvent()    {}
func (ThinkingUpdate) isEvent() {}
func (ToolsUpdate) isEvent()    {}
func (QueueUpdate) isEvent()    {}
func (Settled) isEvent()        {}
func (RuntimeReady) isEvent()   {}
func (SavePoint) isEvent()      {}
func (Abort) isEvent()          {}

// ProviderRetry reports a provider-level retry while the active turn is
// waiting before another request attempt. It is runtime-only and is never
// persisted in the session tree.
type ProviderRetry struct {
	Attempt   int
	Delay     time.Duration
	Err       error
	Timestamp time.Time
}

func (ProviderRetry) isEvent() {}

// Error reports a non-recoverable harness error (e.g., persistence failure).
type Error struct {
	Err error
}

func (*Error) isEvent() {}

// --- Utility functions (stay in session/ as they access domain types). ---

func DeltaText(d Delta) string {
	if d == nil {
		return ""
	}
	if td, ok := d.(TextDelta); ok {
		return td.Text
	}
	return ""
}

// When() methods for core events.

func (s TurnStart) When() time.Time     { return s.Timestamp }
func (s MessageEnd) When() time.Time    { return s.Timestamp }
func (s MessageUpdate) When() time.Time { return s.Timestamp }
