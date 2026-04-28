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
	SessionID string `json:"session_id"`
}

func (e MetadataLoaded) isEvent() {}

// StatusChanged fires when the agent updates its internal status
// or progresses on a long-running step.
type StatusChanged struct {
	Base
	Status string `json:"status"`
}

func (e StatusChanged) isEvent() {}

// PlanUpdated fires when the agent's internal plan of execution changes.
type PlanUpdated struct {
	Base
	Plan string `json:"plan"`
}

func (e PlanUpdated) isEvent() {}

// AgentDelta is an incremental chunk of agent output text.
type AgentDelta struct {
	Base
	Delta string `json:"delta"`
}

func (e AgentDelta) isEvent() {}

// ThinkingDelta is an incremental chunk of agent reasoning/thinking text.
type ThinkingDelta struct {
	Base
	Delta string `json:"delta"`
}

func (e ThinkingDelta) isEvent() {}

// AgentMessage fires when a complete agent message is committed.
type AgentMessage struct {
	Base
	Message   string `json:"message"`
	Reasoning string `json:"reasoning,omitempty"`
}

func (e AgentMessage) isEvent() {}

// ToolCallStarted fires when the agent starts executing a tool.
type ToolCallStarted struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	ToolName  string `json:"tool_name"`
	Args      string `json:"args"`
}

func (e ToolCallStarted) isEvent() {}

// ToolResult fires when the agent finishes executing a tool.
type ToolResult struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	ToolName  string `json:"tool_name"`
	Result    string `json:"result"`
	Error     error  `json:"error,omitempty"`
}

func (e ToolResult) isEvent() {}

// ToolOutputDelta is an incremental chunk of tool output text.
type ToolOutputDelta struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	Delta     string `json:"delta"`
}

func (e ToolOutputDelta) isEvent() {}

// VerificationResult fires when an objective function (test, benchmark,
// compile check) completes. Essential for RLM loops.
type VerificationResult struct {
	Base
	Command string `json:"command"`
	Passed  bool   `json:"passed"`
	Metric  string `json:"metric,omitempty"`
	Output  string `json:"output,omitempty"`
}

func (e VerificationResult) isEvent() {}

// ApprovalRequest fires when the agent needs explicit host approval to continue.
type ApprovalRequest struct {
	Base
	RequestID   string `json:"request_id"`
	Description string `json:"description"`
	ToolName    string `json:"tool_name,omitzero"`
	Args        string `json:"args,omitzero"`
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
	Err   error `json:"err"`
	Fatal bool  `json:"fatal"`
}

func (e Error) isEvent() {}

// ChildRequested fires when the main agent requests a child execution.
type ChildRequested struct {
	Base
	AgentName string `json:"agent_name"`
	Query     string `json:"query"`
}

func (e ChildRequested) isEvent() {}

// ChildStarted fires when the child execution begins.
type ChildStarted struct {
	Base
	AgentName string `json:"agent_name"`
	SessionID string `json:"session_id"`
}

func (e ChildStarted) isEvent() {}

// ChildDelta is an incremental chunk of child subagent output.
type ChildDelta struct {
	Base
	AgentName string `json:"agent_name"`
	Delta     string `json:"delta"`
}

func (e ChildDelta) isEvent() {}

// ChildCompleted fires when the child execution finishes successfully.
type ChildCompleted struct {
	Base
	AgentName string `json:"agent_name"`
	Result    string `json:"result"`
}

func (e ChildCompleted) isEvent() {}

// ChildBlocked fires when the child execution cannot continue without input or approval.
type ChildBlocked struct {
	Base
	AgentName string `json:"agent_name"`
	Reason    string `json:"reason"`
}

func (e ChildBlocked) isEvent() {}

// ChildFailed fires when the child execution fails.
type ChildFailed struct {
	Base
	AgentName string `json:"agent_name"`
	Error     string `json:"error"`
}

func (e ChildFailed) isEvent() {}

// TokenUsage fires when the agent reports its token consumption.
type TokenUsage struct {
	Base
	Input  int     `json:"input"`
	Output int     `json:"output"`
	Total  int     `json:"total"`
	Cost   float64 `json:"cost,omitempty"`
}

func (e TokenUsage) isEvent() {}
