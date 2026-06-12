package session

import (
	"log/slog"
	"time"

	"github.com/go-json-experiment/json"
	"github.com/go-json-experiment/json/jsontext"
	"github.com/nijaru/ion/llm"
	"github.com/oklog/ulid/v2"
)

// EventType identifies the type of an event.
type EventType string

const (
	MessageAdded    EventType = "message_added"
	ContextAdded    EventType = "context_added"
	Handoff         EventType = "handoff"
	ExternalInput   EventType = "external_input"
	LeafMoved       EventType = "leaf_moved"
	BranchSummary   EventType = "branch_summary"
	ModelChanged    EventType = "model_changed"
	ThinkingChanged EventType = "thinking_changed"
	ToolsChanged    EventType = "tools_changed"
	StatusChanged   EventType = "status_changed"
	AgentStarted    EventType = "agent_started"
	AgentCompleted  EventType = "agent_completed"

	// Observability / Lifecycle
	TurnStarted           EventType = "turn_started"
	TurnCompleted         EventType = "turn_completed"
	StepStarted           EventType = "step_started"
	StepCompleted         EventType = "step_completed"
	MessageStarted        EventType = "message_started"
	MessageUpdated        EventType = "message_updated"
	MessageCompleted      EventType = "message_completed"
	ToolStarted           EventType = "tool_started"
	ToolUpdated           EventType = "tool_updated"
	ToolCompleted         EventType = "tool_completed"
	ApprovalRequested     EventType = "approval_requested"
	ApprovalResolved      EventType = "approval_resolved"
	ApprovalCanceled      EventType = "approval_canceled"
	WaitStarted           EventType = "wait_started"
	WaitResolved          EventType = "wait_resolved"
	EscalationRetried     EventType = "escalation_retried"
	CompactionStarted     EventType = "compaction_started"
	CompactionTriggered   EventType = "compaction_triggered"
	ProjectionSnapshotted EventType = "projection_snapshotted"
	ChildRequested        EventType = "child_requested"
	ChildStarted          EventType = "child_started"
	ChildProgressed       EventType = "child_progressed"
	ChildBlocked          EventType = "child_blocked"
	ChildCompleted        EventType = "child_completed"
	ChildFailed           EventType = "child_failed"
	ChildCanceled         EventType = "child_canceled"
	ChildMerged           EventType = "child_merged"
	ArtifactRecorded      EventType = "artifact_recorded"

	// Framework Extensions
	ToolOutputDeltaType EventType = "tool_output_delta"
)

// Event is a single append-only fact in a session.
type Event struct {
	ID        ulid.ULID      `json:"id"`
	SessionID string         `json:"session_id"`
	TurnID    string         `json:"turn_id,omitzero"`
	Seq       int64          `json:"seq,omitzero"`
	ParentID  string         `json:"parent_id,omitzero"`
	Type      EventType      `json:"type"`
	Timestamp time.Time      `json:"timestamp"`
	Data      jsontext.Value `json:"data"`
	Metadata  map[string]any `json:"metadata,omitzero"`
	Cost      float64        `json:"cost,omitzero"`

	metadataRaw jsontext.Value
	message     *llm.Message
}

// UnmarshalData unmarshals the event's data into the given value.
func (e Event) UnmarshalData(v any) error {
	if e.Type == MessageAdded {
		if m, ok := v.(*llm.Message); ok && e.message != nil {
			*m = *e.message
			return nil
		}
	}
	return json.Unmarshal(e.Data, v)
}

func (e *Event) ensureMessage() (*llm.Message, error) {
	if e.Type != MessageAdded {
		return nil, nil
	}
	if e.message != nil {
		return e.message, nil
	}

	var m llm.Message
	if err := json.Unmarshal(e.Data, &m); err != nil {
		return nil, err
	}
	e.message = &m
	return e.message, nil
}

func (e *Event) ensureMetadata() error {
	if e.Metadata != nil || len(e.metadataRaw) == 0 {
		return nil
	}

	var metadata map[string]any
	if err := json.Unmarshal(e.metadataRaw, &metadata); err != nil {
		return err
	}
	e.Metadata = metadata
	return nil
}

func (e Event) encodedMetadata() (jsontext.Value, error) {
	if e.Metadata != nil {
		raw, err := json.Marshal(e.Metadata)
		if err != nil {
			return nil, err
		}
		return raw, nil
	}
	if len(e.metadataRaw) > 0 {
		return e.metadataRaw, nil
	}
	return nil, nil
}

// NewEvent creates a new event with a unique ID and current timestamp.
func NewEvent(sessionID string, eventType EventType, data any) Event {
	raw, err := json.Marshal(data)
	if err != nil {
		slog.Warn("event marshal failed", "error", err)
		raw, _ = json.Marshal(
			map[string]string{"error": "failed to marshal event data: " + err.Error()},
		)
	}
	return Event{
		ID:        ulid.Make(),
		SessionID: sessionID,
		Type:      eventType,
		Timestamp: time.Now().UTC(),
		Data:      raw,
	}
}

// NewMessage creates a new message event.
func NewMessage(sessionID string, msg llm.Message) Event {
	return NewEvent(sessionID, MessageAdded, msg)
}
