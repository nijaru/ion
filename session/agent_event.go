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
type MetadataLoad struct {
	Base
	SessionID string `json:"session_id"`
}

func (e MetadataLoad) isAgentEvent() {}

// StatusChanged fires when the agent updates its internal status
// or progresses on a long-running step.
type StatusChange struct {
	Base
	Status string `json:"status"`
}

func (e StatusChange) isAgentEvent() {}

// AgentStart fires when the agent loop begins processing (Run or Continue).
// Pi equivalent: agent_start.
type AgentStart struct {
	Base
}

func (e AgentStart) isAgentEvent() {}

// AgentEnd fires when the agent loop terminates (shouldStopAfterTurn, error,
// abort, or natural completion). Pi equivalent: agent_end.
type AgentEnd struct {
	Base
}

func (e AgentEnd) isAgentEvent() {}

// AgentDelta is an incremental chunk of agent output text.
type AgentDelta struct {
	Base
	Delta string `json:"delta"`
}

func (e AgentDelta) isAgentEvent() {}

// ThinkingDelta is an incremental chunk of agent reasoning/thinking text.
type ThinkingDelta struct {
	Base
	Delta string `json:"delta"`
}

func (e ThinkingDelta) isAgentEvent() {}

// UserMessage fires when a user message is committed to the durable session.
type UserMessage struct {
	Base
	Message string `json:"message"`
}

func (e UserMessage) isAgentEvent() {}

// AgentMessage fires when a complete agent message is committed.
// Carries token usage (Pi: usage data lives in message_end).
type AgentMessage struct {
	Base
	Message      string `json:"message"`
	Reasoning    string `json:"reasoning,omitempty"`
	InputTokens  int    `json:"input_tokens,omitzero"`
	OutputTokens int    `json:"output_tokens,omitzero"`
	TotalTokens  int    `json:"total_tokens,omitzero"`
	Cost         float64 `json:"cost,omitzero"`
}

func (e AgentMessage) isAgentEvent() {}

// ToolCallStarted fires when the agent starts executing a tool.
type ToolCallStart struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	ToolName  string `json:"tool_name"`
	Args      string `json:"args"`
}

func (e ToolCallStart) isAgentEvent() {}

// ToolResult fires when the agent finishes executing a tool.
type ToolCallEnd struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	ToolName  string `json:"tool_name"`
	Result    string `json:"result"`
	Error     error  `json:"error,omitempty"`
}

func (e ToolCallEnd) isAgentEvent() {}

// ToolOutputDelta is an incremental chunk of tool output text.
type ToolOutputDelta struct {
	Base
	ToolUseID string `json:"tool_use_id,omitempty"`
	Delta     string `json:"delta"`
	Snapshot  bool   `json:"snapshot,omitempty"`
}

func (e ToolOutputDelta) isAgentEvent() {}

// ApprovalRequest is emitted by optional compatibility backends that support
// host-mediated permission prompts. The native Ion path does not emit it.
type ApprovalRequest struct {
	Base
	RequestID   string `json:"request_id"`
	Description string `json:"description"`
	ToolName    string `json:"tool_name,omitzero"`
	Args        string `json:"args,omitzero"`
	Environment string `json:"environment,omitzero"`
}

func (e ApprovalRequest) isAgentEvent() {}

// TurnStarted fires when the backend begins processing a turn.
type TurnStart struct {
	Base
}

func (e TurnStart) isAgentEvent() {}

// TurnFinished fires when the backend has finished its generation cycle for a turn.
type TurnEnd struct {
	Base
}

func (e TurnEnd) isAgentEvent() {}

// Error represents a recoverable or fatal error in the session.
type TurnError struct {
	Base
	Err   error `json:"err"`
	Fatal bool  `json:"fatal"`
}

func (e TurnError) isAgentEvent() {}

// ChildRequested fires when the main agent requests a child execution.
type ChildRequest struct {
	Base
	AgentName string `json:"agent_name"`
	Query     string `json:"query"`
}

func (e ChildRequest) isAgentEvent() {}

// ChildStarted fires when the child execution begins.
type ChildStart struct {
	Base
	AgentName string `json:"agent_name"`
	SessionID string `json:"session_id"`
}

func (e ChildStart) isAgentEvent() {}

// ChildDelta is an incremental chunk of child subagent output.
type ChildDelta struct {
	Base
	AgentName string `json:"agent_name"`
	Delta     string `json:"delta"`
}

func (e ChildDelta) isAgentEvent() {}

// ChildCompleted fires when the child execution finishes successfully.
type ChildComplete struct {
	Base
	AgentName string `json:"agent_name"`
	Result    string `json:"result"`
}

func (e ChildComplete) isAgentEvent() {}

// ChildBlocked fires when the child execution cannot continue without input.
type ChildBlock struct {
	Base
	AgentName string `json:"agent_name"`
	Reason    string `json:"reason"`
}

func (e ChildBlock) isAgentEvent() {}

// ChildFailed fires when the child execution fails.
type ChildFail struct {
	Base
	AgentName string `json:"agent_name"`
	Error     string `json:"error"`
}

func (e ChildFail) isAgentEvent() {}

// ChildCanceled fires when the child execution is canceled.
type ChildCancel struct {
	Base
	AgentName string `json:"agent_name"`
	Reason    string `json:"reason"`
}

func (e ChildCancel) isAgentEvent() {}

// CompactionTrigger fires when context overflow is detected and
// auto-compaction is triggered.
type CompactionTrigger struct {
	Base
	Reason string `json:"reason"` // "overflow" or "threshold"
}

func (e CompactionTrigger) isAgentEvent() {}

// AutoRetryStart fires when auto-retry begins for a transient error.
type AutoRetryStart struct {
	Base
	Attempt    int    `json:"attempt"`
	MaxAttempt int    `json:"max_attempt"`
	DelayMs    int    `json:"delay_ms"`
	Error      string `json:"error"`
}

func (e AutoRetryStart) isAgentEvent() {}

// AutoRetryEnd fires when auto-retry completes (success or failure).
type AutoRetryEnd struct {
	Base
	Success     bool   `json:"success"`
	Attempt     int    `json:"attempt"`
	FinalError  string `json:"final_error,omitempty"`
}

func (e AutoRetryEnd) isAgentEvent() {}

type QueuedInputUpdate struct {
	Base
	Snapshot QueuedInputSnapshot `json:"snapshot"`
}

func (e QueuedInputUpdate) isAgentEvent() {}
