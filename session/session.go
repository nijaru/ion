package session

import (
	"context"
)

// Session is the live session projection used by the current harness.
// The runtime controller is the sole owner of session mutation.
type Session interface {
	ID() string
	Meta() Metadata

	// SessionName returns the user-facing name from the last session_info entry.
	SessionName(ctx context.Context) (string, error)

	// Context projection — the loop consumes this.
	// Reconstructs []Message by walking the branch and extracting MessageEntries.
	BuildContext(ctx context.Context) (ContextSnapshot, error)

	// Branch returns entries on the current leaf path.
	Branch(ctx context.Context) ([]Entry, error)
	// BranchAt returns entries on the explicitly selected leaf path. The leaf
	// is captured by the caller, so the result cannot silently follow a later
	// navigation while a runtime snapshot is being assembled.
	BranchAt(ctx context.Context, leafID string) ([]Entry, error)
	// GetEntry returns one persisted tree entry by ID.
	GetEntry(ctx context.Context, id string) (Entry, error)
	// GetLeafID returns the current durable leaf pointer.
	GetLeafID() string

	// MoveTo switches the leaf pointer to the given entry ID. An empty ID
	// selects the root. Non-empty entries must exist. Optionally appends a
	// branch summary entry at the selected position. Returns the branch summary
	// entry ID if summary was provided, "" otherwise.
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
	AppendLeaf(ctx context.Context, targetID string) (string, error)
	AppendCustomMessage(ctx context.Context, entry *CustomMessageEntry) (string, error)
	Append(ctx context.Context, entry Entry) (string, error)
	// Query.
	Entries(ctx context.Context) ([]Entry, error)
	Usage(ctx context.Context) (Usage, error)
	AppendLabel(ctx context.Context, targetID string, label string) (string, error)
	GetLabel(ctx context.Context, targetID string) (string, error)
}

// ContextSnapshot is the result of BuildContext — what the loop needs to run a turn.
type ContextSnapshot struct {
	Messages       []Message
	ActiveProvider string        // from most recent ModelChangeEntry
	ActiveModel    string        // from most recent ModelChangeEntry
	Thinking       ThinkingLevel // from most recent ThinkingChangeEntry
	ActiveTools    []string      // from most recent ToolsChangeEntry
	ActiveToolsSet bool          // true when a ToolsChangeEntry exists, including an empty set
}

// CompactionData holds the payload for a compaction entry.
type CompactionData struct {
	Summary      string
	FirstKeptID  string
	TokensBefore int
	Usage        Usage
	Details      []byte
}

// BranchSummaryData holds the payload for a branch summary entry.
type BranchSummaryData struct {
	// FromID identifies the leaf being left when the summary was created.
	FromID  string
	Summary string
	Usage   Usage
	Details []byte
}

// an interface for test fakes. The Session façade wraps this.
type Store interface {
	// Append persists an entry. The entry's ID must be set.
	Append(ctx context.Context, entry Entry) (string, error)

	// AppendBatch persists multiple entries atomically. On failure, no entries
	// are persisted (implementations that support transactions will roll back).
	AppendBatch(ctx context.Context, entries []Entry) ([]string, error)

	// AppendLeafEntry appends an entry and atomically updates the leaf pointer
	// to the new entry's ID. Safe under concurrent use.
	AppendLeafEntry(ctx context.Context, entry Entry) (string, error)

	// GetEntry returns a single entry by ID.
	GetEntry(ctx context.Context, id string) (Entry, error)

	// Branch returns all entries on the path from root to the current leaf,
	// in order. This is what buildContext projects into []Message.
	Branch(ctx context.Context) ([]Entry, error)
	// BranchAt reads the branch rooted at an explicit immutable leaf ID.
	BranchAt(ctx context.Context, leafID string) ([]Entry, error)

	// Entries returns all entries in the session (for export/query).
	Entries(ctx context.Context) ([]Entry, error)

	// GetLeafID returns the current leaf entry ID.
	GetLeafID() string

	// SetLeafID moves the leaf pointer.
	SetLeafID(id string) error

	// MoveTo atomically records a navigation, changes the selected leaf, and
	// optionally appends a branch summary. An empty entry ID selects the root.
	// The operation leaves the current leaf unchanged if any part fails.
	MoveTo(ctx context.Context, entryID string, summary *BranchSummaryData) (string, error)

	// Meta returns session-level metadata.
	Meta() Metadata

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
