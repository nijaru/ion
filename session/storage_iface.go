package session

import "context"

// SessionStorage provides the low-level storage interface for session data.
// This matches Pi's SessionStorage interface for parity.
type SessionStorage interface {
	// GetMetadata returns the session's metadata.
	GetMetadata(ctx context.Context) (*Metadata, error)

	// GetLeafID returns the current leaf entry ID.
	GetLeafID(ctx context.Context) (string, error)

	// SetLeafID sets the leaf entry ID (for branching).
	SetLeafID(ctx context.Context, leafID string) error

	// CreateEntryID creates a new unique entry ID.
	CreateEntryID(ctx context.Context) (string, error)

	// AppendEntry appends a new entry to the storage.
	AppendEntry(ctx context.Context, entry TreeEntry) error

	// GetEntry returns an entry by ID.
	GetEntry(ctx context.Context, id string) (*TreeEntry, error)

	// FindEntries returns all entries of the given type.
	FindEntries(ctx context.Context, entryType EntryType) ([]TreeEntry, error)

	// GetLabel returns the label for an entry (if any).
	GetLabel(ctx context.Context, id string) (string, error)

	// GetPathToRoot returns the path from the given leaf to the root.
	GetPathToRoot(ctx context.Context, leafID string) ([]TreeEntry, error)

	// GetEntries returns all entries in the storage.
	GetEntries(ctx context.Context) ([]TreeEntry, error)
}

// SessionRepo handles session lifecycle operations (create, open, list, delete, fork).
// This matches Pi's SessionRepo interface for parity.
type SessionRepo interface {
	// Create creates a new session and returns its storage.
	Create(ctx context.Context, opts CreateSessionOpts) (SessionStorage, error)

	// Open opens an existing session by ID.
	Open(ctx context.Context, id string) (SessionStorage, error)

	// List returns all sessions for the given working directory.
	List(ctx context.Context, cwd string) ([]SessionInfo, error)

	// Delete deletes a session by ID.
	Delete(ctx context.Context, id string) error

	// Fork creates a new session as a fork of an existing session.
	Fork(ctx context.Context, sourceID string, opts ForkSessionOpts) (SessionStorage, error)
}

// CreateSessionOpts contains options for creating a new session.
type CreateSessionOpts struct {
	// Cwd is the working directory for the session.
	Cwd string
	// Model is the initial model for the session.
	Model string
	// Branch is the initial branch name (optional).
	Branch string
}

// ForkSessionOpts contains options for forking a session.
type ForkSessionOpts struct {
	// Label is the label for the fork.
	Label string
	// Reason is the reason for forking.
	Reason string
}
