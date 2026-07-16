package app

import (
	"context"
	"time"
)

// MemoryRecord is the TUI projection of one explicit workspace note. The app
// package intentionally does not depend on the memory storage package.
type MemoryRecord struct {
	ID        string
	Content   string
	Tags      string
	CreatedAt time.Time
	Deleted   bool
}

type MemoryController interface {
	Search(ctx context.Context, query string, includeDeleted bool, limit int) ([]MemoryRecord, error)
	Delete(ctx context.Context, id string) error
	Restore(ctx context.Context, id string) error
}
