package session

// Event is the base interface for all strongly typed session events.
// These are decoupled from the host UI (e.g. Bubble Tea) and represent
// the domain model of a native or ACP agent session.
type Event interface {
	isSessionEvent()
}

// BaseEvent provides common fields for events in a swarm/multi-agent session.
type BaseEvent struct {
	AgentID string // Identifies the sub-agent or worker (empty for the main agent)
	TraceID string // Identifies a specific execution branch or task tree
}

// EventMetadataLoaded fires when a session's metadata is loaded or created.
type EventMetadataLoaded struct {
	BaseEvent
	SessionID string
}

func (e EventMetadataLoaded) isSessionEvent() {}

// EventStatusChanged fires when the agent updates its internal status
// or progresses on a long-running step.
type EventStatusChanged struct {
	BaseEvent
	Status string
}

func (e EventStatusChanged) isSessionEvent() {}

// EventPlanUpdated fires when the agent's internal plan of execution changes.
type EventPlanUpdated struct {
	BaseEvent
	Plan string
}

func (e EventPlanUpdated) isSessionEvent() {}

// EventAssistantDelta is an incremental chunk of assistant output text.
type EventAssistantDelta struct {
	BaseEvent
	Delta string
}

func (e EventAssistantDelta) isSessionEvent() {}

// EventAssistantMessage fires when a complete assistant message is committed.
type EventAssistantMessage struct {
	BaseEvent
	Message string
}

func (e EventAssistantMessage) isSessionEvent() {}

// EventToolCallStarted fires when the agent starts executing a tool.
type EventToolCallStarted struct {
	BaseEvent
	ToolName string
	Args     string
}

func (e EventToolCallStarted) isSessionEvent() {}

// EventToolResult fires when the agent finishes executing a tool.
type EventToolResult struct {
	BaseEvent
	ToolName string
	Result   string
	Error    error
}

func (e EventToolResult) isSessionEvent() {}

// EventToolOutputDelta is an incremental chunk of tool output text.
type EventToolOutputDelta struct {
	BaseEvent
	Delta string
}

func (e EventToolOutputDelta) isSessionEvent() {}

// EventVerificationResult fires when an objective function (test, benchmark, 
// compile check) completes. Essential for RLM loops.
type EventVerificationResult struct {
	BaseEvent
	Command string
	Passed  bool
	Metric  string // e.g., "val_loss: 0.12" or "42/42 tests"
	Output  string
}

func (e EventVerificationResult) isSessionEvent() {}

// EventApprovalRequest fires when the agent needs explicit host approval to continue.
type EventApprovalRequest struct {
	BaseEvent
	RequestID   string
	Description string
}

func (e EventApprovalRequest) isSessionEvent() {}

// EventTurnStarted fires when the backend begins processing a turn.
type EventTurnStarted struct {
	BaseEvent
}

func (e EventTurnStarted) isSessionEvent() {}

// EventTurnFinished fires when the backend has finished its generation cycle for a turn.
type EventTurnFinished struct {
	BaseEvent
}

func (e EventTurnFinished) isSessionEvent() {}

// EventError represents a recoverable or fatal error in the session.
type EventError struct {
	BaseEvent
	Error error
	Fatal bool
}

func (e EventError) isSessionEvent() {}
