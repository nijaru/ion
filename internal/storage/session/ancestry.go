package session

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/oklog/ulid/v2"
)

// SessionAncestry records the tree relationship of a persisted session.
type SessionAncestry struct {
	SessionID        string    `json:"session_id"`
	ParentSessionID  string    `json:"parent_session_id,omitzero"`
	ForkPointEventID string    `json:"fork_point_event_id,omitzero"`
	BranchLabel      string    `json:"branch_label,omitzero"`
	ForkReason       string    `json:"fork_reason,omitzero"`
	Depth            int       `json:"depth"`
	CreatedAt        time.Time `json:"created_at"`
}

// ForkOptions carries optional metadata for a forked session branch.
type ForkOptions struct {
	BranchLabel string
	ForkReason  string
}

// SessionTreeStore exposes persisted session-tree queries.
type SessionTreeStore interface {
	Parent(ctx context.Context, sessionID string) (*SessionAncestry, error)
	Children(ctx context.Context, sessionID string) ([]SessionAncestry, error)
	Lineage(ctx context.Context, sessionID string) ([]SessionAncestry, error)
}

// SessionAncestryWriter persists existing ancestry metadata for portable
// session imports.
type SessionAncestryWriter interface {
	SaveAncestry(ctx context.Context, record SessionAncestry) error
}

// ForkStore materializes forked sessions from persisted parent session IDs,
// with optional ancestry metadata such as branch labels or fork reasons.
type ForkStore interface {
	ForkWithOptions(
		ctx context.Context,
		originalSessionID, newSessionID string,
		opts ForkOptions,
	) (*Session, error)
}

// SessionBranchStore materializes a persisted child branch from the current
// in-memory parent session.
type SessionBranchStore interface {
	BranchSession(
		ctx context.Context,
		parent *Session,
		newSessionID string,
		opts ForkOptions,
	) (*Session, error)
}

func maxULID() ulid.ULID {
	return ulid.MustParse("7ZZZZZZZZZZZZZZZZZZZZZZZZZ")
}

func reverseAncestry(items []SessionAncestry) {
	for i, j := 0, len(items)-1; i < j; i, j = i+1, j-1 {
		items[i], items[j] = items[j], items[i]
	}
}

func validateSessionAncestry(record SessionAncestry) error {
	if strings.TrimSpace(record.SessionID) == "" {
		return fmt.Errorf("session ancestry missing session id")
	}
	if record.CreatedAt.IsZero() {
		return fmt.Errorf("session ancestry %q missing created_at", record.SessionID)
	}
	return nil
}

func lineageFromMap(
	sessionID string,
	records map[string]SessionAncestry,
) ([]SessionAncestry, error) {
	lineage := make([]SessionAncestry, 0, 8)
	seen := make(map[string]struct{}, 8)
	current := sessionID

	for current != "" {
		record, ok := records[current]
		if !ok {
			return nil, fmt.Errorf("session ancestry %q not found", current)
		}
		if _, exists := seen[current]; exists {
			return nil, fmt.Errorf("session ancestry cycle at %q", current)
		}
		seen[current] = struct{}{}
		lineage = append(lineage, record)
		current = record.ParentSessionID
	}

	reverseAncestry(lineage)
	return lineage, nil
}
