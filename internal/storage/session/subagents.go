package session

import "github.com/nijaru/ion/internal/llm"

// ChildMode identifies how a child session was created.
type ChildMode string

const (
	ChildModeFork    ChildMode = "fork"
	ChildModeHandoff ChildMode = "handoff"
	ChildModeFresh   ChildMode = "fresh"
)

// ChildStatus captures the high-level lifecycle state of a child run.
type ChildStatus string

const (
	ChildStatusRequested ChildStatus = "requested"
	ChildStatusRunning   ChildStatus = "running"
	ChildStatusBlocked   ChildStatus = "blocked"
	ChildStatusCompleted ChildStatus = "completed"
	ChildStatusFailed    ChildStatus = "failed"
	ChildStatusCanceled  ChildStatus = "canceled"
	ChildStatusMerged    ChildStatus = "merged"
)

// ArtifactKindWorkspaceFileRef marks durable file-reference records that
// should stay internal to the framework and not surface as child artifacts.
const ArtifactKindWorkspaceFileRef = "workspace_file_ref"

// ChildRequestedData records a parent request for a child run.
type ChildRequestedData struct {
	ChildID         string         `json:"child_id"`
	ChildSessionID  string         `json:"child_session_id"`
	ParentEventID   string         `json:"parent_event_id,omitzero"`
	AgentID         string         `json:"agent_id"`
	Mode            ChildMode      `json:"mode"`
	Task            string         `json:"task"`
	Context         string         `json:"context,omitzero"`
	SharedPrefixKey string         `json:"shared_prefix_key,omitzero"`
	Metadata        map[string]any `json:"metadata,omitzero"`
}

// ChildStartedData records that a child run has started execution.
type ChildStartedData struct {
	ChildID        string         `json:"child_id"`
	ChildSessionID string         `json:"child_session_id"`
	AgentID        string         `json:"agent_id"`
	Metadata       map[string]any `json:"metadata,omitzero"`
}

// ChildProgressedData records an application-defined child status update.
type ChildProgressedData struct {
	ChildID        string         `json:"child_id"`
	ChildSessionID string         `json:"child_session_id"`
	Status         string         `json:"status,omitzero"`
	Message        string         `json:"message,omitzero"`
	Metadata       map[string]any `json:"metadata,omitzero"`
}

// ChildBlockedData records that a child run cannot continue without input or approval.
type ChildBlockedData struct {
	ChildID        string         `json:"child_id"`
	ChildSessionID string         `json:"child_session_id"`
	Reason         string         `json:"reason"`
	Metadata       map[string]any `json:"metadata,omitzero"`
}

// ChildCompletedData records the durable outcome of a completed child run.
type ChildCompletedData struct {
	ChildID        string         `json:"child_id"`
	ChildSessionID string         `json:"child_session_id"`
	Summary        string         `json:"summary,omitzero"`
	ArtifactIDs    []string       `json:"artifact_ids,omitzero"`
	EpisodeID      string         `json:"episode_id,omitzero"`
	Usage          llm.Usage      `json:"usage,omitzero"`
	Metadata       map[string]any `json:"metadata,omitzero"`
}

// ChildFailedData records that a child run failed.
type ChildFailedData struct {
	ChildID        string         `json:"child_id"`
	ChildSessionID string         `json:"child_session_id"`
	Error          string         `json:"error"`
	Metadata       map[string]any `json:"metadata,omitzero"`
}

// ChildCanceledData records that a child run was canceled.
type ChildCanceledData struct {
	ChildID        string         `json:"child_id"`
	ChildSessionID string         `json:"child_session_id"`
	Reason         string         `json:"reason,omitzero"`
	Metadata       map[string]any `json:"metadata,omitzero"`
}

// ChildMergedData records that an application merged a child outcome into parent flow.
type ChildMergedData struct {
	ChildID        string         `json:"child_id"`
	ChildSessionID string         `json:"child_session_id"`
	ArtifactIDs    []string       `json:"artifact_ids,omitzero"`
	Note           string         `json:"note,omitzero"`
	Metadata       map[string]any `json:"metadata,omitzero"`
}

// ArtifactRecordedData records that a session or child run emitted an artifact.
type ArtifactRecordedData struct {
	ChildID   string      `json:"child_id,omitzero"`
	Artifact  ArtifactRef `json:"artifact"`
	SessionID string      `json:"session_id,omitzero"`
}

func NewChildRequestedEvent(sessionID string, data ChildRequestedData) Event {
	return NewEvent(sessionID, ChildRequested, data)
}

func NewChildStartedEvent(sessionID string, data ChildStartedData) Event {
	return NewEvent(sessionID, ChildStarted, data)
}

func NewChildProgressedEvent(sessionID string, data ChildProgressedData) Event {
	return NewEvent(sessionID, ChildProgressed, data)
}

func NewChildBlockedEvent(sessionID string, data ChildBlockedData) Event {
	return NewEvent(sessionID, ChildBlocked, data)
}

func NewChildCompletedEvent(sessionID string, data ChildCompletedData) Event {
	return NewEvent(sessionID, ChildCompleted, data)
}

func NewChildFailedEvent(sessionID string, data ChildFailedData) Event {
	return NewEvent(sessionID, ChildFailed, data)
}

func NewChildCanceledEvent(sessionID string, data ChildCanceledData) Event {
	return NewEvent(sessionID, ChildCanceled, data)
}

func NewChildMergedEvent(sessionID string, data ChildMergedData) Event {
	return NewEvent(sessionID, ChildMerged, data)
}

func NewArtifactRecordedEvent(sessionID string, data ArtifactRecordedData) Event {
	if data.SessionID == "" {
		data.SessionID = sessionID
	}
	return NewEvent(sessionID, ArtifactRecorded, data)
}

func (e Event) ChildRequestedData() (ChildRequestedData, bool, error) {
	return decodeEventData[ChildRequestedData](e, ChildRequested, "child requested")
}

func (e Event) ChildStartedData() (ChildStartedData, bool, error) {
	return decodeEventData[ChildStartedData](e, ChildStarted, "child started")
}

func (e Event) ChildProgressedData() (ChildProgressedData, bool, error) {
	return decodeEventData[ChildProgressedData](e, ChildProgressed, "child progressed")
}

func (e Event) ChildBlockedData() (ChildBlockedData, bool, error) {
	return decodeEventData[ChildBlockedData](e, ChildBlocked, "child blocked")
}

func (e Event) ChildCompletedData() (ChildCompletedData, bool, error) {
	return decodeEventData[ChildCompletedData](e, ChildCompleted, "child completed")
}

func (e Event) ChildFailedData() (ChildFailedData, bool, error) {
	return decodeEventData[ChildFailedData](e, ChildFailed, "child failed")
}

func (e Event) ChildCanceledData() (ChildCanceledData, bool, error) {
	return decodeEventData[ChildCanceledData](e, ChildCanceled, "child canceled")
}

func (e Event) ChildMergedData() (ChildMergedData, bool, error) {
	return decodeEventData[ChildMergedData](e, ChildMerged, "child merged")
}

func (e Event) ArtifactRecordedData() (ArtifactRecordedData, bool, error) {
	return decodeEventData[ArtifactRecordedData](e, ArtifactRecorded, "artifact recorded")
}

// IsWorkspaceFileReferenceArtifact reports whether ref is an internal
// framework record for a file identity seen during prompt construction.
func IsWorkspaceFileReferenceArtifact(ref ArtifactRef) bool {
	return ref.Kind == ArtifactKindWorkspaceFileRef
}
