package session

import (
	"context"
)

// Session is the live session handle. The harness is the only writer.
// Translated from Pi's Session contract (agent-harness.js usage).
type Session interface {
	ID() string
	Meta() Metadata

	// Context projection — the loop consumes this.
	// Reconstructs []Message by walking the branch and extracting MessageEntries.
	BuildContext(ctx context.Context) (ContextSnapshot, error)

	// Branch returns entries on the current leaf path.
	Branch(ctx context.Context) ([]Entry, error)

	// MoveTo switches the leaf pointer to the given entry ID.
	// The entry must exist. Optionally appends a branch summary entry.
	// Returns the branch summary entry ID if summary was provided, "" otherwise.
	MoveTo(ctx context.Context, entryID string, summary *BranchSummaryData) (string, error)

	// Typed append methods — each creates the right entry kind.
	AppendMessage(ctx context.Context, msg Message) (string, error)
	AppendCompaction(ctx context.Context, data CompactionData) (string, error)
	AppendBranchSummary(ctx context.Context, data BranchSummaryData) (string, error)
	AppendSessionInfo(ctx context.Context, name string) (string, error)
	AppendModelChange(ctx context.Context, provider string, modelID string) (string, error)
	AppendThinkingLevelChange(ctx context.Context, level ThinkingLevel) (string, error)
	AppendActiveToolsChange(ctx context.Context, tools []string) (string, error)
	AppendCustom(ctx context.Context, entry *CustomEntry) (string, error)
	Append(ctx context.Context, entry Entry) (string, error)
	Events() <-chan Event
	EventSender() chan Event

	// Query.
	Entries(ctx context.Context) ([]Entry, error)
	Usage(ctx context.Context) (Usage, error)

	Close() error
}

// ContextSnapshot is the result of BuildContext — what the loop needs to run a turn.
type ContextSnapshot struct {
	Messages    []Message
	ActiveModel string        // from most recent ModelChangeEntry
	Thinking    ThinkingLevel // from most recent ThinkingChangeEntry
	ActiveTools []string      // from most recent ToolsChangeEntry
}

// CompactionData holds the payload for a compaction entry.
type CompactionData struct {
	Summary      string
	FirstKeptID  string
	TokensBefore int
	Details      []byte
}

// BranchSummaryData holds the payload for a branch summary entry.
type BranchSummaryData struct {
	Summary string
	Details []byte
}

func (s *sessionImpl) Events() <-chan Event { return nil }

// an interface for test fakes. The Session façade wraps this.
type Store interface {
	// Append persists an entry. The entry's ID must be set.
	Append(ctx context.Context, entry Entry) (string, error)

	// AppendBatch persists multiple entries atomically. On failure, no entries
	// are persisted (implementations that support transactions will roll back).
	AppendBatch(ctx context.Context, entries []Entry) ([]string, error)

	// GetEntry returns a single entry by ID.
	GetEntry(ctx context.Context, id string) (Entry, error)

	// Branch returns all entries on the path from root to the current leaf,
	// in order. This is what buildContext projects into []Message.
	Branch(ctx context.Context) ([]Entry, error)

	// Entries returns all entries in the session (for export/query).
	Entries(ctx context.Context) ([]Entry, error)

	// GetLeafID returns the current leaf entry ID.
	GetLeafID() string

	// SetLeafID moves the leaf pointer.
	SetLeafID(id string) error

	// GetMetadata returns session-level metadata.
	GetMetadata() Metadata
	Meta() Metadata

	// GetInputs returns queued user inputs.
	GetInputs(ctx context.Context, workdir string, n int) ([]string, error)

	// ListSessions returns all sessions for the picker.
	ListSessions(ctx context.Context, workdir string) ([]SessionInfoEntry, error)

	// UpdateSession updates session metadata.
	UpdateSession(ctx context.Context, info SessionInfoEntry) error

	// AddInput adds a queued user input.
	AddInput(ctx context.Context, workdir string, input string) error

	// Close releases resources.
	Close() error
}

// Metadata holds session-level state.
type Metadata struct {
	CWD    string
	Model  string
	Branch string
	ID     string // session identifier
	Name   string // user-facing name (set via AppendSessionInfo)
}

// ResumeSession loads an existing session by ID.
func ResumeSession(ctx context.Context, store Store, sessionID string) (Store, string, error) {
	_, err := store.GetEntry(ctx, sessionID)
	if err != nil {
		return nil, "", err
	}
	if err := store.SetLeafID(sessionID); err != nil {
		return nil, "", err
	}
	return store, sessionID, nil
}
