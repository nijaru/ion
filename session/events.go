package session

import (
	"context"
	"fmt"
	"time"
)

// Event is the closed union of events the agent loop and harness emit.
// The loop emits the lifecycle + streaming + tool-execution events (Event Agent
// interface methods tag the source). The harness emits session/control events.
//
// The taxonomy is collapsed from Pi's shape: streaming is message_start /
// message_update (carrying a Delta union) / message_end — NOT a 9-way split of
// text/thinking/toolcall start/delta/end. Adding an event is a design change.
type Event interface {
	isEvent()
}

// --- Loop-emitted (the loop's sole output channel). ---

// AgentStart opens a run. Origin tags root vs child for the future subagent seam.
type AgentStart struct {
	Origin SessionOrigin
}

// TurnStart opens a turn (one assistant response + its tool execution).
type TurnStart struct{}

// MessageStart opens a message (user prompt, assistant response, or tool result).
type MessageStart struct {
	Message Message
}

// MessageUpdate carries an incremental update to the streaming assistant message.
// The Delta union is the collapsed form of Pi's text/thinking/toolcall deltas.
type MessageUpdate struct {
	Message Message // the accumulated partial message
	Delta   Delta
}

// MessageEnd closes a message; Message is final.
type MessageEnd struct {
	Message Message
}

// ToolExecStart opens execution of one tool call.
type ToolExecStart struct {
	ToolCallID string
	Name       string
	Args       []byte // raw JSON arguments
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
	Error error
	Base  EventBase
	Message     Message // the assistant message that ended the turn
	ToolResults []ToolResultMessage
}

// AgentEnd closes a run. Invariant: exactly one AgentEnd per logical turn,
// regardless of retry attempts (attempts emit AutoRetryStart/End instead).
type AgentEnd struct {
	Messages []Message
}

// Delta is the sealed union of streaming deltas (collapses the 9-way split).
type Delta interface {
	isDelta()
}

type TextDelta struct {
	Text string
}

type ThinkingDelta struct {
	Text string
}

// ToolCallDelta streams a tool-call's arguments as they arrive (partial JSON).
type ToolCallDelta struct {
	ToolCallID    string
	Name          string
	ArgumentsChunk string
}

func (TextDelta) isDelta()       {}
func (ThinkingDelta) isDelta()   {}
func (ToolCallDelta) isDelta()   {}

// --- Harness-emitted (session/control). ---

// QueueUpdate reports the pending steer/followUp/nextTurn queues (for TUI display).
type QueueUpdate struct {
	Steer    []Message
	FollowUp []Message
	NextTurn []Message
}

// ModelUpdate reports a model switch (runtime or buffered-then-applied).
type ModelUpdate struct {
	Model, Previous string
}

// ThinkingUpdate reports a thinking-level switch.
type ThinkingUpdate struct {
	Level, Previous ThinkingLevel
}

// ToolsUpdate reports an active-tools change.
type ToolsUpdate struct {
	Active, Previous []string
}

// CompactionTrigger signals compaction was triggered (proactive or overflow recovery).
type CompactionTrigger struct {
	Reason string // "proactive" | "overflow" | "manual"
}

// AutoRetryStart opens an overflow-recovery attempt (compact + retry).
type AutoRetryStart struct {
	Reason string
}

// AutoRetryEnd closes a recovery attempt. Exactly one terminal AgentEnd follows
// the final attempt — AutoRetryEnd is not itself an AgentEnd.
type AutoRetryEnd struct {
	Success    bool
	FinalError string
}

// SessionCompacted reports a completed compaction.
type SessionCompacted struct {
	Entry CompactionEntry
}

// SessionTreeMoved reports a branch navigation.
type SessionTreeMoved struct {
	NewLeafID, OldLeafID string
}

// Settled signals the harness returned to idle and all listeners have drained.
type Settled struct{}

// Error reports a non-recoverable harness error.
type Error struct {
	Err error
}

// --- Sealing. ---

func (AgentStart) isEvent()       {}
func (TurnStart) isEvent()        {}
func (MessageStart) isEvent()     {}
func (MessageUpdate) isEvent()    {}
func (MessageEnd) isEvent()       {}
func (ToolExecStart) isEvent()    {}
func (ToolExecUpdate) isEvent()   {}
func (ToolExecEnd) isEvent()      {}
func (TurnEnd) isEvent()          {}
func (AgentEnd) isEvent()         {}
func (QueueUpdate) isEvent()      {}
func (ModelUpdate) isEvent()      {}
func (ThinkingUpdate) isEvent()   {}
func (ToolsUpdate) isEvent()      {}
func (CompactionTrigger) isEvent(){}
func (AutoRetryStart) isEvent()   {}
func (AutoRetryEnd) isEvent()     {}
func (SessionCompacted) isEvent() {}
func (SessionTreeMoved) isEvent() {}
func (Settled) isEvent()          {}
func (*Error) isEvent()           {}

// ToolPartial is a progress payload from a running tool (opaque to the loop).
type ToolPartial = any

// SessionOrigin identifies which session an event belongs to. The root session
// is the user's; a child session identifies a future subagent run. This is the
// subagent seam — present now, unused until subagents ship.
type SessionOrigin struct {
	SessionID string
	ChildID   string // non-empty for subagent-originated events
}

// App-facing types — app/ uses these until its rewrite against the new Event taxonomy.

type StoreEvent = Entry

type StoreRoutingDecision struct {
	EntryBase
	Type           string
	Decision       string
	Reason         string
	ModelSlot      string
	Provider       string
	Model          string
	Reasoning      string
	MaxSessionCost float64
	MaxTurnCost    float64
	SessionCost    float64
	TurnCost       float64
	StopReason     string
	TS             time.Time
}
func (StoreRoutingDecision) isEvent() {}

type SubmitPreflightDecision struct {
	Allowed bool
	ShouldSubmit bool
	Reason       string
}

type SteeringSession interface {
	SteerTurn(ctx context.Context, text string) (SteeringResult, error)
}

type QueuedInputSession interface {
	FollowUpTurn(ctx context.Context, text string) (FollowUpResult, error)
	ClearQueuedInput(ctx context.Context) (string, error)
}

type StatusChange struct {
	EntryBase
	Status string
}
func (StatusChange) isEvent() {}

type SessionTree struct {
	Current  Entry
	Lineage  []Entry
	Children []Entry
}

type SessionTreeReader interface {
	SessionTree(ctx context.Context, leafID string) (SessionTree, error)
}

func IsMaterialized(s Session) bool { return true }

type QueuedInputUpdate struct{ EntryBase }
func (QueuedInputUpdate) isEvent() {}

type AgentMessage = Message

type ToolCallStart struct {
	EntryBase
	ToolCallID string
	Name       string
	Args       []byte
}
func (ToolCallStart) isEvent() {}

type ToolExecutionUpdate struct {
	EntryBase
	ToolCallID string
	Partial    ToolPartial
}
func (ToolExecutionUpdate) isEvent() {}

type ToolCallEnd struct {
	EntryBase
	ToolCallID string
	Result     ToolResultMessage
}
func (ToolCallEnd) isEvent() {}

type SessionBundle struct {
	RootSessionID string
	Sessions      []SessionBundleRecord
	ExportedAt    time.Time
}

type SessionBundleRecord struct {
	Info   Session
	Events []Entry
}

type SessionBundleExporter interface {
	ExportSessionBundle(ctx context.Context, leafID string) (SessionBundle, error)
}

type SessionBundleImporter interface {
	ImportSessionBundle(ctx context.Context, bundle SessionBundle) (string, error)
}

const RoleAgent = "agent"

type SubmitPreflightInput struct {
	RuntimeRequired bool
	Provider        string
	Model           string
	TotalCost       float64
	MaxSessionCost  float64
	MaxTurnCost     float64
}

func DecideSubmitPreflight(input SubmitPreflightInput) SubmitPreflightDecision {
	return SubmitPreflightDecision{Allowed: true}
}

var budgetStopReasonStr = "budget_stop"

func BudgetStopReason(input BudgetStopInput) string { return budgetStopReasonStr }

type BudgetStopInput struct {
	Reason         string
	CurrentTurnCost float64
	TotalCost      float64
	MaxTurnCost    float64
	MaxSessionCost float64
}

// Additional fields needed by app/ model_status.go

func IsConversationSessionInfo(e Entry) bool {
	_, ok := e.(*SessionInfoEntry)
	return ok
}

type DisplayError struct {
	EntryBase
	Err error
}
func (DisplayError) isEvent() {}

func RouteBusyInput(input BusyInputRouting) string { return input.Route }

type BusyInputRouting struct {
	Mode              string
	Thinking          bool
	Compacting        bool
	SupportsSteering  bool
	SupportsFollowUp  bool
	Route string
}

var BusyInputRouteSteer = "steer"
var BusyInputRouteFollowUp = "follow_up"

func (e StoreRoutingDecision) ID() string      { return e.EntryBase.ID }
func (e StoreRoutingDecision) ParentID() string { return e.EntryBase.ParentID }
func (e StoreRoutingDecision) When() time.Time  { return e.EntryBase.Timestamp }


type FollowUpResultInput struct{
	Text               string
	PriorFollowUpCount int
	CurrentFollowUp    []string
	Result             FollowUpResult
	Err                error
}

func (StoreRoutingDecision) isEntry() {}

type SteeringResult struct{}
type FollowUpResult struct{}

type BusyInputDecision struct {
	Recall        bool
	ComposerText  string
	ClearBackend  bool
	Action         string
	NoticeContent  string
	FollowUp       []string
}

var BusyInputResultAccepted = "accepted"

func DecideSteeringResult(result SteeringResult, err error) BusyInputDecision {
	if err != nil {
		return BusyInputDecision{}
	}
	return BusyInputDecision{Action: BusyInputResultAccepted, NoticeContent: "Steering applied"}
}

func DecideFollowUpResult(input FollowUpResultInput) BusyInputDecision {
	if input.Err != nil {
		return BusyInputDecision{}
	}
	return BusyInputDecision{Action: BusyInputResultAccepted, NoticeContent: "Follow-up queued", FollowUp: []string{input.Text}}
}

type QueuedInputRecallInput struct{
	Text         string
	CurrentDraft string
	Steering     []string
	FollowUp     []string
	BackendOwned bool
}

func DecideQueuedInputRecall(input QueuedInputRecallInput) BusyInputDecision {
	return BusyInputDecision{Action: BusyInputResultAccepted, NoticeContent: "Input recalled"}
}


type EventBase struct {
	Timestamp time.Time
	Error     error
}

func BaseNow() EventBase { return EventBase{Timestamp: time.Now()} }
func BaseAt(t time.Time) EventBase { return EventBase{Timestamp: t} }

type EventDrainInput struct {
	Active    bool
	DrainStartedAt time.Time
	Event     Event
}

type EventDrainDecision struct {
	Drain       bool
	Action      string
	FinishDrain bool
}

var EventDrainAwait = "await"

func DecideEventDrain(input EventDrainInput) EventDrainDecision {
	return EventDrainDecision{Drain: input.Active}
}

type StoreSystem struct {
	EntryBase
	Type    string
	Content string
	TS      int64
}
func (StoreSystem) isEntry() {}
func (s StoreSystem) ID() string { return s.EntryBase.ID }
func (s StoreSystem) ParentID() string { return s.EntryBase.ParentID }
func (s StoreSystem) When() time.Time { return s.EntryBase.Timestamp }

type ChildRequest struct{ EntryBase; AgentID string }
func (ChildRequest) isEvent() {}
type ChildStart struct{ EntryBase; AgentID string }
func (ChildStart) isEvent() {}
type ChildDelta struct{ EntryBase; AgentID string; Delta Delta }
func (ChildDelta) isEvent() {}
type ChildComplete struct{ EntryBase; AgentID string }
func (ChildComplete) isEvent() {}
type ChildBlock struct{ EntryBase; AgentID string; Reason string }
func (ChildBlock) isEvent() {}
type ChildCancel struct{ EntryBase; AgentID string }
func (ChildCancel) isEvent() {}
type ChildFail struct{ EntryBase; AgentID string; Err error }
func (ChildFail) isEvent() {}

func EntryUser(content string, ts time.Time) (*MessageEntry, string) {
	id := fmt.Sprintf("%d", ts.UnixNano())
	return &MessageEntry{
		EntryBase: EntryBase{ID: id, Timestamp: ts},
		Message:   NewUserText(content, ts),
	}, id
}

type ErrorSettlementInput struct {
	AwaitTerminal bool
	Err       error
	Thinking  bool
	Compacting bool
}

type ErrorSettlementDecision struct {
	RoutingStop   bool
	PersistSystem bool
	AwaitNext     bool
	DisplayError string
	EntryContent string
}

func DecideErrorSettlement(input ErrorSettlementInput) ErrorSettlementDecision {
	return ErrorSettlementDecision{DisplayError: input.Err.Error()}
}

type StoreStatus struct {
	EntryBase
	Status    string
	Timestamp time.Time
}
func (StoreStatus) isEntry() {}
func (s StoreStatus) ID() string { return s.EntryBase.ID }
func (s StoreStatus) ParentID() string { return s.EntryBase.ParentID }
func (s StoreStatus) When() time.Time { return s.EntryBase.Timestamp }
