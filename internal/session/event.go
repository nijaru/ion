package session

// Event is the base interface for all strongly typed session events.
// These are decoupled from the host UI (e.g. Bubble Tea) and represent
// the domain model of a native or ACP agent session.
type Event interface {
	isEvent()
}

// Base provides common fields for events in a swarm/multi-agent session.
type Base struct {
	AgentID string // Identifies the sub-agent or worker (empty for the main agent)
	TraceID string // Identifies a specific execution branch or task tree
}

// MetadataLoaded fires when a session's metadata is loaded or created.
type MetadataLoaded struct {
	Base
	SessionID string
}

func (e MetadataLoaded) isEvent() {}

// StatusChanged fires when the agent updates its internal status
// or progresses on a long-running step.
type StatusChanged struct {
	Base
	Status string
}

func (e StatusChanged) isEvent() {}

// PlanUpdated fires when the agent's internal plan of execution changes.
type PlanUpdated struct {
	Base
	Plan string
}

func (e PlanUpdated) isEvent() {}

// AssistantDelta is an incremental chunk of assistant output text.
type AssistantDelta struct {
	Base
	Delta string
}

func (e AssistantDelta) isEvent() {}

// ThinkingDelta is an incremental chunk of assistant reasoning/thinking text.
type ThinkingDelta struct {
	Base
	Delta string
}

func (e ThinkingDelta) isEvent() {}

// AssistantMessage fires when a complete assistant message is committed.
type AssistantMessage struct {
	Base
	Message string
}

func (e AssistantMessage) isEvent() {}

// ToolCallStarted fires when the agent starts executing a tool.
type ToolCallStarted struct {
	Base
	ToolName string
	Args     string
}

func (e ToolCallStarted) isEvent() {}

// ToolResult fires when the agent finishes executing a tool.
type ToolResult struct {
	Base
	ToolName string
	Result   string
	Error    error
}

func (e ToolResult) isEvent() {}

// ToolOutputDelta is an incremental chunk of tool output text.
type ToolOutputDelta struct {
	Base
	Delta string
}

func (e ToolOutputDelta) isEvent() {}

// VerificationResult fires when an objective function (test, benchmark, 
// compile check) completes. Essential for RLM loops.
type VerificationResult struct {
	Base
	Command string
	Passed  bool
	Metric  string // e.g., "val_loss: 0.12" or "42/42 tests"
	Output  string
}

func (e VerificationResult) isEvent() {}

// ApprovalRequest fires when the agent needs explicit host approval to continue.
type ApprovalRequest struct {
	Base
	RequestID   string
	Description string
}

func (e ApprovalRequest) isEvent() {}

// TurnStarted fires when the backend begins processing a turn.
type TurnStarted struct {
	Base
}

func (e TurnStarted) isEvent() {}

// TurnFinished fires when the backend has finished its generation cycle for a turn.
type TurnFinished struct {
	Base
}

func (e TurnFinished) isEvent() {}

// Error represents a recoverable or fatal error in the session.
type Error struct {
	Base
	Err   error
	Fatal bool
}

func (e Error) isEvent() {}
