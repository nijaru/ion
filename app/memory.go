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
	Audit(ctx context.Context, limit int) ([]MemoryAuditRecord, error)
	Delete(ctx context.Context, id string) error
	Restore(ctx context.Context, id string) error
}

type MemoryAuditRecord struct {
	Sequence  int64
	MemoryID  string
	Operation string
	Content   string
	Tags      string
	At        time.Time
}

type memorySearchMsg struct {
	requestID      uint64
	query          string
	includeDeleted bool
	records        []MemoryRecord
	err            error
}

type memoryAuditMsg struct {
	requestID uint64
	entries   []MemoryAuditRecord
	err       error
}

type memoryActionMsg struct {
	requestID uint64
	action    string
	id        string
	err       error
}
