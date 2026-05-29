package session

import (
	"context"
	"fmt"
	"time"
)

// Fork creates a new session by copying all events from an existing session.
func (s *JSONLStore) Fork(ctx context.Context, originalID, newID string) (*Session, error) {
	return s.ForkWithOptions(ctx, originalID, newID, ForkOptions{})
}

// ForkWithOptions creates a new session by copying all events from an existing
// session and records session-level ancestry metadata in the JSONL index.
func (s *JSONLStore) ForkWithOptions(
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
// parent session, preserving copied history and ancestry metadata on disk.
func (s *JSONLStore) BranchSession(
	_ context.Context,
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

	childMu := s.getSessionMu(newID)
	childMu.Lock()
	defer childMu.Unlock()

	s.ancestryMu.Lock()
	defer s.ancestryMu.Unlock()
	parentDepth, err := s.ensureRootAncestryLocked(parentID, parentCreatedAt)
	if err != nil {
		return nil, err
	}
	if err := s.appendAncestryLocked(SessionAncestry{
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
		if err := s.saveLocked(e); err != nil {
			return nil, err
		}
	}
	return forked, nil
}
