package session

import "time"

// Event is the closed union of events the agent loop and harness emit.
// Matches Pi's 10 events from agent-loop.js:
//   agent_start, turn_start, message_start, message_update, message_end,
//   tool_execution_start, tool_execution_update, tool_execution_end,
//   turn_end, agent_end
//
// message_update carries a Delta union (text/thinking/toolcall).
type Event interface {
	IsEvent()
}

// --- Core events (Pi-aligned). ---

// AgentStart opens a run. Origin tags root vs child for the future subagent seam.
type AgentStart struct {
	Origin SessionOrigin
}

// TurnStart opens a turn (one assistant response + its tool execution).
type TurnStart struct {
	Timestamp time.Time
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

func (AgentStart) IsEvent()    {}
func (TurnStart) IsEvent()     {}
func (MessageStart) IsEvent()  {}
func (MessageUpdate) IsEvent() {}
func (MessageEnd) IsEvent()    {}
func (ToolExecStart) IsEvent() {}
func (ToolExecUpdate) IsEvent(){}
func (ToolExecEnd) IsEvent()   {}
func (TurnEnd) IsEvent()       {}
func (AgentEnd) IsEvent()      {}

// --- Harness lifecycle events (Pi-aligned). ---

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
// Reference: Pi agent-harness.js emitQueueUpdate (line 249).
type QueueUpdate struct {
	Steer    []Message
	FollowUp []Message
	NextTurn []Message
}

// Settled is emitted after agent_end, signaling the harness is back to idle.
// Reference: Pi agent-harness.js (line ~487).
type Settled struct {
	NextTurnCount int
}

// SavePoint is emitted after turn_end flush, signaling all writes are durable.
// Reference: Pi agent-harness.js (line ~480).
type SavePoint struct {
	HadPendingMutations bool
}

// Abort is emitted when the current run is aborted, carrying cleared queues.
// Reference: Pi agent-harness.js (line ~905).
type Abort struct {
	ClearedSteer    []Message
	ClearedFollowUp []Message
}

func (ModelUpdate) IsEvent()    {}
func (ThinkingUpdate) IsEvent() {}
func (ToolsUpdate) IsEvent()    {}
func (QueueUpdate) IsEvent()    {}
func (Settled) IsEvent()        {}
func (SavePoint) IsEvent()      {}
func (Abort) IsEvent()          {}

// AfterProviderResponse is emitted when the provider responds (HTTP response received).
// Fires after before_provider_request/payload hooks and before the loop processes the
// assistant stream. Status and Headers reflect the HTTP response when available;
// both are zero/nil when the provider abstraction does not expose them.
// Reference: Pi agent-harness.js streamSimple onResponse callback (line ~327).
type AfterProviderResponse struct {
	Status  int
	Headers map[string]string
}

// Error reports a non-recoverable harness error (e.g., persistence failure).
type Error struct {
	Err error
}

func (AfterProviderResponse) IsEvent() {}
func (*Error) IsEvent()              {}

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
