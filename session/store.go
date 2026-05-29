package session

import (
	"context"

	"github.com/oklog/ulid/v2"
)

// Store is an interface for persisting session state.
type Store interface {
	Save(ctx context.Context, e Event) error
	Load(ctx context.Context, sessionID string) (*Session, error)
	// LoadUntil loads a session up to (and including) the given event ID.
	LoadUntil(ctx context.Context, sessionID string, eventID ulid.ULID) (*Session, error)
	// Fork creates a new session by copying all events from an existing session.
	Fork(ctx context.Context, originalSessionID, newSessionID string) (*Session, error)
}

// SearchStore exposes full-text search over persisted session events.
// Not every store implements this capability.
type SearchStore interface {
	Search(ctx context.Context, sessionID string, query string) ([]Event, error)
}

// EventQueryStore exposes sequence-bounded reads over persisted session events.
// Hosts use this to keep derived projections current without loading the full
// session log on every read.
type EventQueryStore interface {
	EventsAfter(ctx context.Context, sessionID string, afterSeq int64) ([]Event, error)
}

var (
	_ SessionTreeStore   = (*SQLiteStore)(nil)
	_ ForkStore          = (*SQLiteStore)(nil)
	_ SessionBranchStore = (*SQLiteStore)(nil)
	_ EventQueryStore    = (*SQLiteStore)(nil)
	_ SessionTreeStore   = (*JSONLStore)(nil)
	_ ForkStore          = (*JSONLStore)(nil)
	_ SessionBranchStore = (*JSONLStore)(nil)
	_ EventQueryStore    = (*JSONLStore)(nil)
)
