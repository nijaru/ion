package session

import (
	"context"
	"fmt"
	"time"
)

// Fork creates a new session by copying all events from an existing session.
func (s *SQLiteStore) Fork(ctx context.Context, originalID, newID string) (*Session, error) {
	return s.ForkWithOptions(ctx, originalID, newID, ForkOptions{})
}

// ForkWithOptions creates a new session by copying all events from an existing
// session and persists session-level ancestry metadata for the child.
func (s *SQLiteStore) ForkWithOptions(
	ctx context.Context,
	originalID, newID string,
	opts ForkOptions,
) (*Session, error) {
	sess, err := s.Load(ctx, originalID)
	if err != nil {
		return nil, err
	}

	return s.BranchSession(ctx, sess, newID, opts)
}

// BranchSession creates a persisted child branch from the current in-memory
// parent session, preserving copied history and ancestry metadata in SQLite.
func (s *SQLiteStore) BranchSession(
	ctx context.Context,
	parent *Session,
	newID string,
	opts ForkOptions,
) (*Session, error) {
	if parent == nil {
		return nil, fmt.Errorf("fork live session %q: nil parent", newID)
	}
	parentID := parent.ID()
	forked, forkPointEventID := parent.forkWithOrigin(newID)
	parentCreatedAt := sessionCreatedAt(parent)
	childCreatedAt := time.Now().UTC()

	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()

	parentDepth, err := ensureSQLiteRootAncestryTx(ctx, tx, parentID, parentCreatedAt)
	if err != nil {
		return nil, err
	}
	if err := saveSQLiteAncestryTx(ctx, tx, SessionAncestry{
		SessionID:        newID,
		ParentSessionID:  parentID,
		ForkPointEventID: forkPointEventID,
		BranchLabel:      opts.BranchLabel,
		ForkReason:       opts.ForkReason,
		Depth:            parentDepth + 1,
		CreatedAt:        childCreatedAt,
	}); err != nil {
		return nil, err
	}
	for _, e := range forked.Events() {
		if err := s.saveTx(ctx, tx, e); err != nil {
			return nil, err
		}
	}

	if err := tx.Commit(); err != nil {
		return nil, err
	}

	return forked, nil
}

func sessionCreatedAt(sess *Session) time.Time {
	events := sess.Events()
	if len(events) == 0 {
		return time.Now().UTC()
	}
	return events[0].Timestamp
}
