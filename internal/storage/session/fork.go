package session

import (
	"context"
	"crypto/rand"
	"errors"
	"log/slog"

	"github.com/go-json-experiment/json"
	"github.com/oklog/ulid/v2"
)

// Branch creates a persisted child branch from the current in-memory parent
// session, including copied history and ancestry metadata.
func (s *Session) Branch(
	ctx context.Context,
	newID string,
	opts ForkOptions,
) (*Session, error) {
	s.mu.RLock()
	writer := s.writer
	s.mu.RUnlock()

	if writer == nil {
		return nil, errors.New("branch session: session has no durable writer")
	}
	store, ok := writer.(SessionBranchStore)
	if !ok {
		return nil, errors.New(
			"branch session: writer does not support branching from a live session",
		)
	}
	return store.BranchSession(ctx, s, newID, opts)
}

// Fork creates a new session with a new ID, copying all existing events from
// this session. The subscribers are not copied.
func (s *Session) Fork(newID string) *Session {
	forked, _ := s.forkWithOrigin(newID)
	return forked
}

func (s *Session) forkWithOrigin(newID string) (*Session, string) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	forkPointEventID := ""
	if n := len(s.events); n > 0 {
		forkPointEventID = s.events[n-1].ID.String()
	}
	activeLeafID := s.activeLeafID
	events := make([]Event, len(s.events))
	entropy := ulid.Monotonic(rand.Reader, 0)
	idMap := make(map[string]string, len(s.events))
	for i, e := range s.events {
		if err := e.ensureMetadata(); err != nil {
			slog.Warn("fork metadata decode failed", "event_id", e.ID, "error", err)
		}
		originSessionID := e.SessionID
		originEventID := e.ID
		e.ID = ulid.MustNew(ulid.Timestamp(e.Timestamp), entropy)
		idMap[originEventID.String()] = e.ID.String()
		e.SessionID = newID
		e.Metadata = cloneMetadata(e.Metadata)
		e.Metadata["fork_origin"] = ForkOrigin{
			SessionID: originSessionID,
			EventID:   originEventID.String(),
		}.metadataValue()
		events[i] = e
	}
	for i, e := range events {
		if e.ParentID != "" {
			e.ParentID = idMap[e.ParentID]
		}
		events[i] = remapForkedEventData(e, idMap)
	}
	if activeLeafID != "" {
		activeLeafID = idMap[activeLeafID]
	}

	res := &Session{
		id:           newID,
		events:       events,
		activeLeafID: activeLeafID,
		nextSeq:      s.nextSeq,
		state:        make(map[string]any, len(s.state)),
		writer:       s.writer,
		reducer:      s.reducer,
	}
	if res.nextSeq == 0 {
		res.nextSeq = int64(len(events) + 1)
	}
	for k, v := range s.state {
		res.state[k] = v
	}
	return res, forkPointEventID
}

func cloneMetadata(src map[string]any) map[string]any {
	if len(src) == 0 {
		return make(map[string]any)
	}

	dst := make(map[string]any, len(src))
	for k, v := range src {
		dst[k] = v
	}
	return dst
}

func remapForkedEventData(e Event, idMap map[string]string) Event {
	leaf, ok, err := e.LeafMovedData()
	if err == nil && ok {
		if newID, ok := idMap[leaf.TargetEventID]; ok {
			leaf.TargetEventID = newID
			if rewritten, marshalErr := json.Marshal(leaf); marshalErr == nil {
				e.Data = rewritten
			}
		}
		return e
	}

	snapshot, ok, err := e.ProjectionSnapshot()
	if err == nil && ok {
		rewritten, marshalErr := json.Marshal(remapCompactionSnapshot(snapshot, idMap))
		if marshalErr == nil {
			e.Data = rewritten
		}
		return e
	}
	snapshot, ok, err = e.CompactionSnapshot()
	if err != nil || !ok {
		return e
	}

	rewritten, marshalErr := json.Marshal(remapCompactionSnapshot(snapshot, idMap))
	if marshalErr != nil {
		return e
	}
	e.Data = rewritten
	return e
}
