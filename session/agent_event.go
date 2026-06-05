package session

import "time"

// Event is the base interface for all strongly typed session events.
// These are decoupled from the host UI (e.g. Bubble Tea) and represent
// the domain model of a native or ACP agent session.
type AgentEvent interface {
	isAgentEvent()
}

// Base provides common fields for events in a swarm/multi-agent session.
type Base struct {
	Timestamp time.Time `json:"timestamp,omitzero"` // UTC event time, zero when unknown
	AgentID   string    `json:"agent_id,omitzero"`  // sub-agent/worker, empty for main agent
	TraceID   string    `json:"trace_id,omitzero"`  // execution branch or task tree
}

func BaseAt(timestamp time.Time) Base {
	if timestamp.IsZero() {
		return Base{}
	}
	return Base{Timestamp: timestamp.UTC()}
}

func BaseNow() Base {
	return BaseAt(time.Now())
}

// MetadataLoaded fires when a session's metadata is loaded or created.
type MetadataLoadedEvent struct {
	Base
	SessionID string `json:"session_id"`
}

func (e MetadataLoadedEvent) isAgentEvent() {}

// StatusChanged fires when the agent updates its internal status
// or progresses on a long-running step.
type StatusChangedEvent struct {
	Base
	Status string `json:"status"`
}

func (e StatusChangedEvent) isAgentEvent() {}

// PlanUpdated fires when the agent's internal plan of execution changes.
type PlanUpdatedEvent struct {
	Base
	Plan string `json:"plan"`
}

func (e PlanUpdatedEvent) isAgentEvent() {}

// AgentDelta is an incremental chunk of agent output text.
type AgentDeltaEvent struct {
	Base
	Delta string `json:"delta"`
}

func (e AgentDeltaEvent) isAgentEvent() {}

// ThinkingDelta is an incremental chunk of agent reasoning/thinking text.
type ThinkingDeltaEvent struct {
	Base
	Delta string `json:"delta"`
}

func (e ThinkingDeltaEvent) isAgentEvent() {}

// UserMessage fires when a user message is committed to the durable session.
type UserMessageEvent struct {
	Base
	Message string `json:"message"`
}

func (e UserMessageEvent) isAgentEvent() {}

// AgentMessage fires when a complete agent message is committed.
type AgentMessageEvent struct {
	Base
	Message   string `json:"message"`
	Reasoning string `json:"reasoning,omitempty"`
}

func (e AgentMessageEvent) isAgentEvent() {}

// ToolCallStarted fires when the agent starts executing a tool.
type ToolCallStartedEvent struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	ToolName  string `json:"tool_name"`
	Args      string `json:"args"`
}

func (e ToolCallStartedEvent) isAgentEvent() {}

// ToolResult fires when the agent finishes executing a tool.
type ToolResultEvent struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	ToolName  string `json:"tool_name"`
	Result    string `json:"result"`
	Error     error  `json:"error,omitempty"`
}

func (e ToolResultEvent) isAgentEvent() {}

// ToolOutputDelta is an incremental chunk of tool output text.
type ToolOutputDeltaEvent struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	Delta     string `json:"delta"`
	Snapshot  bool   `json:"snapshot,omitempty"`
}

func (e ToolOutputDeltaEvent) isAgentEvent() {}

// VerificationResult fires when an objective function (test, benchmark,
// compile check) completes. Essential for RLM loops.
type VerificationResultEvent struct {
	Base
	Command string `json:"command"`
	Passed  bool   `json:"passed"`
	Metric  string `json:"metric,omitempty"`
	Output  string `json:"output,omitempty"`
}

func (e VerificationResultEvent) isAgentEvent() {}

// ApprovalRequest is emitted by optional compatibility backends that support
// host-mediated permission prompts. The native Ion path does not emit it.
type ApprovalRequestEvent struct {
	Base
	RequestID   string `json:"request_id"`
	Description string `json:"description"`
	ToolName    string `json:"tool_name,omitzero"`
	Args        string `json:"args,omitzero"`
	Environment string `json:"environment,omitzero"`
}

func (e ApprovalRequestEvent) isAgentEvent() {}

// TurnStarted fires when the backend begins processing a turn.
type TurnStartedEvent struct {
	Base
}

func (e TurnStartedEvent) isAgentEvent() {}

// TurnFinished fires when the backend has finished its generation cycle for a turn.
type TurnFinishedEvent struct {
	Base
}

func (e TurnFinishedEvent) isAgentEvent() {}

// TurnSavePoint fires when the backend has flushed durable writes for a turn.
type TurnSavePointEvent struct {
	Base
	HadPendingMutations bool `json:"had_pending_mutations,omitempty"`
}

func (e TurnSavePointEvent) isAgentEvent() {}

// Error represents a recoverable or fatal error in the session.
type ErrorEvent struct {
	Base
	Err   error `json:"err"`
	Fatal bool  `json:"fatal"`
}

func (e ErrorEvent) isAgentEvent() {}

// ChildRequested fires when the main agent requests a child execution.
type ChildRequestedEvent struct {
	Base
	AgentName string `json:"agent_name"`
	Query     string `json:"query"`
}

func (e ChildRequestedEvent) isAgentEvent() {}

// ChildStarted fires when the child execution begins.
type ChildStartedEvent struct {
	Base
	AgentName string `json:"agent_name"`
	SessionID string `json:"session_id"`
}

func (e ChildStartedEvent) isAgentEvent() {}

// ChildDelta is an incremental chunk of child subagent output.
type ChildDeltaEvent struct {
	Base
	AgentName string `json:"agent_name"`
	Delta     string `json:"delta"`
}

func (e ChildDeltaEvent) isAgentEvent() {}

// ChildCompleted fires when the child execution finishes successfully.
type ChildCompletedEvent struct {
	Base
	AgentName string `json:"agent_name"`
	Result    string `json:"result"`
}

func (e ChildCompletedEvent) isAgentEvent() {}

// ChildBlocked fires when the child execution cannot continue without input.
type ChildBlockedEvent struct {
	Base
	AgentName string `json:"agent_name"`
	Reason    string `json:"reason"`
}

func (e ChildBlockedEvent) isAgentEvent() {}

// ChildFailed fires when the child execution fails.
type ChildFailedEvent struct {
	Base
	AgentName string `json:"agent_name"`
	Error     string `json:"error"`
}

func (e ChildFailedEvent) isAgentEvent() {}

// ChildCanceled fires when the child execution is canceled.
type ChildCanceledEvent struct {
	Base
	AgentName string `json:"agent_name"`
	Reason    string `json:"reason"`
}

func (e ChildCanceledEvent) isAgentEvent() {}

// TokenUsage fires when the agent reports its token consumption.
type TokenUsageEvent struct {
	Base
	Input  int     `json:"input"`
	Output int     `json:"output"`
	Total  int     `json:"total"`
	Cost   float64 `json:"cost,omitempty"`
}

func (e TokenUsageEvent) isAgentEvent() {}

// CompactionTriggeredEvent fires when context overflow is detected and
// auto-compaction is triggered.
type CompactionTriggeredEvent struct {
	Base
	Reason string `json:"reason"` // "overflow" or "threshold"
}

func (e CompactionTriggeredEvent) isAgentEvent() {}

// AutoRetryStartEvent fires when auto-retry begins for a transient error.
type AutoRetryStartEvent struct {
	Base
	Attempt    int    `json:"attempt"`
	MaxAttempt int    `json:"max_attempt"`
	DelayMs    int    `json:"delay_ms"`
	Error      string `json:"error"`
}

func (e AutoRetryStartEvent) isAgentEvent() {}

// AutoRetryEndEvent fires when auto-retry completes (success or failure).
type AutoRetryEndEvent struct {
	Base
	Success     bool   `json:"success"`
	Attempt     int    `json:"attempt"`
	FinalError  string `json:"final_error,omitempty"`
}

func (e AutoRetryEndEvent) isAgentEvent() {}

type QueuedInputUpdatedEvent struct {
	Base
	Snapshot QueuedInputSnapshot `json:"snapshot"`
}

func (e QueuedInputUpdatedEvent) isAgentEvent() {}
