package session

// Event is the base interface for all strongly typed session events.
// These are decoupled from the host UI (e.g. Bubble Tea) and represent
// the domain model of a native or ACP agent session.
type Event interface {
	isSessionEvent()
}

// EventMetadataLoaded fires when a session's metadata is loaded or created.
type EventMetadataLoaded struct {
	SessionID string
}

func (e EventMetadataLoaded) isSessionEvent() {}

// EventStatusChanged fires when the agent updates its internal status
// or progresses on a long-running step.
type EventStatusChanged struct {
	Status string
}

func (e EventStatusChanged) isSessionEvent() {}

// EventPlanUpdated fires when the agent's internal plan of execution changes.
type EventPlanUpdated struct {
	Plan string
}

func (e EventPlanUpdated) isSessionEvent() {}

// EventAssistantDelta is an incremental chunk of assistant output text.
type EventAssistantDelta struct {
	Delta string
}

func (e EventAssistantDelta) isSessionEvent() {}

// EventAssistantMessage fires when a complete assistant message is committed.
type EventAssistantMessage struct {
	Message string
}

func (e EventAssistantMessage) isSessionEvent() {}

// EventToolCallStarted fires when the agent starts executing a tool.
type EventToolCallStarted struct {
	ToolName string
	Args     string
}

func (e EventToolCallStarted) isSessionEvent() {}

// EventToolResult fires when the agent finishes executing a tool.
type EventToolResult struct {
	ToolName string
	Result   string
	Error    error
}

func (e EventToolResult) isSessionEvent() {}

// EventApprovalRequest fires when the agent needs explicit host approval to continue.
type EventApprovalRequest struct {
	RequestID   string
	Description string
}

func (e EventApprovalRequest) isSessionEvent() {}

// EventTurnStarted fires when the backend begins processing a turn.
type EventTurnStarted struct{}

func (e EventTurnStarted) isSessionEvent() {}

// EventTurnFinished fires when the backend has finished its generation cycle for a turn.
type EventTurnFinished struct{}

func (e EventTurnFinished) isSessionEvent() {}

// EventError represents a recoverable or fatal error in the session.
type EventError struct {
	Error error
	Fatal bool
}

func (e EventError) isSessionEvent() {}
