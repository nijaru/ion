package session

import (
	"fmt"

	"github.com/go-json-experiment/json"

	"github.com/nijaru/ion/internal/llm"
)

// HistoryEntry captures a model-visible message together with its originating
// message event ID when one exists.
type HistoryEntry struct {
	EventID          string           `json:"event_id,omitzero"`
	EventType        EventType        `json:"event_type,omitzero"`
	ContextKind      ContextKind      `json:"context_kind,omitzero"`
	ContextPlacement ContextPlacement `json:"placement,omitzero"`
	Message          llm.Message      `json:"message"`
	Tool             *ToolHistory     `json:"tool,omitzero"`
}

// ToolHistory carries durable tool lifecycle metadata associated with a
// model-visible tool-result message. It is projection metadata for hosts and
// UIs; provider-visible prompt construction still uses Message.
type ToolHistory struct {
	ID             string `json:"id,omitzero"`
	Name           string `json:"name,omitzero"`
	Arguments      string `json:"args,omitzero"`
	IdempotencyKey string `json:"idempotency_key,omitzero"`
	IsError        bool   `json:"is_error,omitzero"`
	Error          string `json:"error,omitzero"`
}

// CompactionSnapshot captures the model-visible history after a compaction step.
type CompactionSnapshot struct {
	Strategy      string         `json:"strategy"`
	MaxTokens     int            `json:"max_tokens,omitzero"`
	ThresholdPct  float64        `json:"threshold_pct,omitzero"`
	CurrentTokens int            `json:"current_tokens,omitzero"`
	CutoffEventID string         `json:"cutoff_event_id,omitzero"`
	Entries       []HistoryEntry `json:"entries,omitzero"`
	Messages      []llm.Message  `json:"messages,omitzero"`
	// ReadFiles tracks file paths the agent read during this compaction window.
	ReadFiles []string `json:"read_files,omitzero"`
	// ModifiedFiles tracks file paths the agent edited or wrote during this
	// compaction window.
	ModifiedFiles []string `json:"modified_files,omitzero"`
}

// CompactionStartedData records that a compaction strategy has begun.
type CompactionStartedData struct {
	Strategy      string  `json:"strategy"`
	MaxTokens     int     `json:"max_tokens,omitzero"`
	ThresholdPct  float64 `json:"threshold_pct,omitzero"`
	CurrentTokens int     `json:"current_tokens,omitzero"`
}

// ForkOrigin identifies the parent event copied into a forked session.
type ForkOrigin struct {
	SessionID string `json:"session_id"`
	EventID   string `json:"event_id"`
}

func (o ForkOrigin) metadataValue() map[string]any {
	return map[string]any{
		"session_id": o.SessionID,
		"event_id":   o.EventID,
	}
}

// NewCompactionEvent records a durable compaction snapshot in the session log.
func NewCompactionEvent(sessionID string, snapshot CompactionSnapshot) Event {
	return NewEvent(sessionID, CompactionTriggered, snapshot)
}

// NewCompactionStartedEvent records the start of a compaction strategy.
func NewCompactionStartedEvent(sessionID string, data CompactionStartedData) Event {
	return NewEvent(sessionID, CompactionStarted, data)
}

// CompactionStartedData decodes the payload of a compaction-started event.
func (e Event) CompactionStartedData() (CompactionStartedData, bool, error) {
	if e.Type != CompactionStarted {
		return CompactionStartedData{}, false, nil
	}

	var data CompactionStartedData
	if err := e.UnmarshalData(&data); err != nil {
		return CompactionStartedData{}, true, fmt.Errorf(
			"decode compaction started event %s: %w",
			e.ID,
			err,
		)
	}
	return data, true, nil
}

// CompactionSnapshot decodes the payload of a compaction event.
func (e Event) CompactionSnapshot() (CompactionSnapshot, bool, error) {
	if e.Type != CompactionTriggered {
		return CompactionSnapshot{}, false, nil
	}

	var snapshot CompactionSnapshot
	if err := e.UnmarshalData(&snapshot); err != nil {
		return CompactionSnapshot{}, true, fmt.Errorf("decode compaction event %s: %w", e.ID, err)
	}
	return snapshot, true, nil
}

// ProjectionSnapshot decodes the payload of a projection snapshot event.
func (e Event) ProjectionSnapshot() (ProjectionSnapshot, bool, error) {
	if e.Type != ProjectionSnapshotted {
		return ProjectionSnapshot{}, false, nil
	}

	var snapshot ProjectionSnapshot
	if err := e.UnmarshalData(&snapshot); err != nil {
		return ProjectionSnapshot{}, true, fmt.Errorf("decode projection event %s: %w", e.ID, err)
	}
	return snapshot, true, nil
}

// ForkOrigin decodes the fork lineage metadata attached to a copied event.
func (e Event) ForkOrigin() (ForkOrigin, bool, error) {
	raw, ok := e.Metadata["fork_origin"]
	if !ok {
		return ForkOrigin{}, false, nil
	}

	data, err := json.Marshal(raw)
	if err != nil {
		return ForkOrigin{}, true, fmt.Errorf("marshal fork origin for event %s: %w", e.ID, err)
	}

	var origin ForkOrigin
	if err := json.Unmarshal(data, &origin); err != nil {
		return ForkOrigin{}, true, fmt.Errorf("decode fork origin for event %s: %w", e.ID, err)
	}
	return origin, true, nil
}

// EffectiveMessages returns the model-visible session history after applying
// the latest durable compaction or projection snapshot, if any.
func (s *Session) EffectiveMessages() ([]llm.Message, error) {
	return NewRebuilder().RebuildMessages(s)
}

func (s *Session) rawMessagesLocked() ([]llm.Message, error) {
	events, err := s.activeEventsLocked()
	if err != nil {
		return nil, err
	}
	res := make([]llm.Message, 0, len(events)/2+1)
	for i := range events {
		e := &events[i]
		if e.Type != MessageAdded {
			continue
		}

		m, err := e.ensureMessage()
		if err != nil {
			return nil, fmt.Errorf("effective history: decode raw message %s: %w", e.ID, err)
		}
		res = append(res, *m)
	}
	return res, nil
}

// EffectiveEntries returns the model-visible session history after applying
// the latest durable compaction or projection snapshot, together with the
// originating event ID for each message when known.
func (s *Session) EffectiveEntries() ([]HistoryEntry, error) {
	return NewRebuilder().RebuildEntries(s)
}

func (s *Session) rawEntriesLocked() ([]HistoryEntry, error) {
	events, err := s.activeEventsLocked()
	if err != nil {
		return nil, err
	}
	return rawEntriesFromEvents(events)
}

func rawEntriesFromEvents(events []Event) ([]HistoryEntry, error) {
	res := make([]HistoryEntry, 0, len(events)/2+1)
	for i := range events {
		e := &events[i]
		if e.Type != MessageAdded && e.Type != ContextAdded && e.Type != BranchSummary {
			continue
		}

		entry, err := historyEntryFromEvent(e)
		if err != nil {
			return nil, fmt.Errorf("effective history: decode raw message %s: %w", e.ID, err)
		}
		res = append(res, entry)
	}
	return res, nil
}

func (s *Session) historyEntryFromEvent(e *Event) (HistoryEntry, error) {
	return historyEntryFromEvent(e)
}

func historyEntryFromEvent(e *Event) (HistoryEntry, error) {
	if e.Type == ContextAdded {
		entry, err := e.ensureContextEntry()
		if err != nil {
			return HistoryEntry{}, err
		}
		return HistoryEntry{
			EventID:          e.ID.String(),
			EventType:        ContextAdded,
			ContextKind:      entry.Kind,
			ContextPlacement: entry.Placement,
			Message:          contextEntryMessage(*entry),
		}, nil
	}
	if e.Type == BranchSummary {
		summary, ok, err := e.BranchSummaryData()
		if err != nil {
			return HistoryEntry{}, err
		}
		if !ok {
			return HistoryEntry{}, nil
		}
		return HistoryEntry{
			EventID:   e.ID.String(),
			EventType: BranchSummary,
			Message:   branchSummaryMessage(summary),
		}, nil
	}

	msg, err := e.ensureMessage()
	if err != nil {
		return HistoryEntry{}, err
	}
	return HistoryEntry{
		EventID:   e.ID.String(),
		EventType: MessageAdded,
		Message:   normalizeTranscriptMessage(*msg),
	}, nil
}

func (s *Session) latestDurableSnapshot() (CompactionSnapshot, bool, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.latestDurableSnapshotLocked()
}

func (s *Session) latestDurableSnapshotLocked() (CompactionSnapshot, bool, error) {
	activeEvents, err := s.activeEventsLocked()
	if err != nil {
		return CompactionSnapshot{}, false, err
	}
	return latestDurableSnapshotFromEvents(activeEvents)
}

func latestDurableSnapshotFromEvents(events []Event) (CompactionSnapshot, bool, error) {
	for i := len(events) - 1; i >= 0; i-- {
		snapshot, ok, err := events[i].ProjectionSnapshot()
		if err != nil {
			return CompactionSnapshot{}, false, err
		}
		if ok {
			if snapshot.CutoffEventID == "" ||
				(len(snapshot.Entries) == 0 && len(snapshot.Messages) == 0) {
				continue
			}
			return snapshot, true, nil
		}

		snapshot, ok, err = events[i].CompactionSnapshot()
		if err != nil {
			return CompactionSnapshot{}, false, err
		}
		if !ok {
			continue
		}
		if snapshot.CutoffEventID == "" ||
			(len(snapshot.Entries) == 0 && len(snapshot.Messages) == 0) {
			continue
		}
		return snapshot, true, nil
	}

	return CompactionSnapshot{}, false, nil
}

func (s CompactionSnapshot) messages() []llm.Message {
	if len(s.Messages) > 0 {
		return s.Messages
	}
	messages := make([]llm.Message, 0, len(s.Entries))
	for _, entry := range s.Entries {
		messages = append(messages, entry.Message)
	}
	return messages
}

func (s CompactionSnapshot) entries() []HistoryEntry {
	if len(s.Entries) > 0 {
		return s.Entries
	}
	entries := make([]HistoryEntry, 0, len(s.Messages))
	for _, msg := range s.Messages {
		entries = append(entries, HistoryEntry{Message: msg})
	}
	return entries
}

func remapCompactionSnapshot(
	snapshot CompactionSnapshot,
	idMap map[string]string,
) CompactionSnapshot {
	if newID, ok := idMap[snapshot.CutoffEventID]; ok {
		snapshot.CutoffEventID = newID
	}
	if len(snapshot.Entries) == 0 {
		return snapshot
	}

	entries := make([]HistoryEntry, 0, len(snapshot.Entries))
	for _, entry := range snapshot.Entries {
		if newID, ok := idMap[entry.EventID]; ok {
			entry.EventID = newID
		}
		entries = append(entries, entry)
	}
	snapshot.Entries = entries
	return snapshot
}
