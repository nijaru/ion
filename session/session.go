package session

import "context"

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

	// Typed append methods — each creates the right entry kind.
	AppendMessage(ctx context.Context, msg Message) (string, error)
	AppendModelChange(ctx context.Context, provider, modelID string) (string, error)
	AppendThinkingChange(ctx context.Context, level ThinkingLevel) (string, error)
	AppendToolsChange(ctx context.Context, tools []string) (string, error)
	AppendCompaction(ctx context.Context, data CompactionData) (string, error)
	AppendBranchSummary(ctx context.Context, data BranchSummaryData) (string, error)
	AppendLabel(ctx context.Context, targetID, label string) (string, error)
	AppendSessionInfo(ctx context.Context, name string) (string, error)
	AppendCustom(ctx context.Context, entry *CustomEntry) (string, error)

	// Tree navigation.
	GetEntry(ctx context.Context, id string) (Entry, error)
	GetLeafID() string
	SetLeafID(id string) error
	MoveTo(ctx context.Context, leafID string, summary *BranchSummaryData) (summaryID string, err error)

	// Query.
	Entries(ctx context.Context) ([]Entry, error)
	Usage(ctx context.Context) (Usage, error)

	Close() error
}

// ContextSnapshot is the result of BuildContext — what the loop needs to run a turn.
type ContextSnapshot struct {
	Messages []Message
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
