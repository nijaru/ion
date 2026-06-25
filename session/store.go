package session

import "context"

// Store is the low-level persistence seam. One concrete impl (SQLiteStore);
// an interface for test fakes. The Session façade wraps this.
type Store interface {
	// Append persists an entry. The entry's ID must be set.
	Append(ctx context.Context, entry Entry) error

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

	// Close releases resources.
	Close() error
}

// Metadata holds session-level state.
type Metadata struct {
	ID   string // session identifier
	Name string // user-facing name (set via AppendSessionInfo)
}
