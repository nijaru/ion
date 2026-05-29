package session

import (
	"context"
	"time"

	"github.com/nijaru/ion/llm"
)

// ProjectionSnapshot reuses the durable compaction snapshot payload shape for
// time/count rebuild checkpoints. Projection snapshots are a durable
// acceleration layer beside replay, not a separate mutable state model.
type ProjectionSnapshot = CompactionSnapshot

const (
	defaultProjectionMaxEvents = 50
)

// ProjectionTrigger records why a projection snapshot was taken.
type ProjectionTrigger string

const (
	ProjectionTriggerCount  ProjectionTrigger = "count"
	ProjectionTriggerTime   ProjectionTrigger = "time"
	ProjectionTriggerManual ProjectionTrigger = "manual"
)

// ProjectionSnapshotter appends durable rebuild checkpoints when a session has
// grown large enough by event count or wall-clock age.
type ProjectionSnapshotter struct {
	MaxEvents int
	MaxAge    time.Duration
	Rebuilder *Rebuilder
	Now       func() time.Time
}

// NewProjectionSnapshotter creates a snapshotter with the default rebuilder.
func NewProjectionSnapshotter() *ProjectionSnapshotter {
	return &ProjectionSnapshotter{
		MaxEvents: defaultProjectionMaxEvents,
		Rebuilder: NewRebuilder(),
	}
}

// NewProjectionSnapshot records a durable projection snapshot in the session
// log.
func NewProjectionSnapshot(sessionID string, snapshot ProjectionSnapshot) Event {
	return NewEvent(sessionID, ProjectionSnapshotted, snapshot)
}

// SnapshotIfNeeded appends a durable projection snapshot when the configured
// count or age policy says the current session should be checkpointed.
func (s *ProjectionSnapshotter) SnapshotIfNeeded(
	ctx context.Context,
	sess *Session,
) (bool, error) {
	trigger, ok, err := s.shouldSnapshot(sess)
	if err != nil || !ok {
		return false, err
	}
	return s.snapshot(ctx, sess, trigger)
}

// Snapshot appends a durable projection snapshot unconditionally.
func (s *ProjectionSnapshotter) Snapshot(
	ctx context.Context,
	sess *Session,
) (bool, error) {
	return s.snapshot(ctx, sess, ProjectionTriggerManual)
}

func (s *ProjectionSnapshotter) snapshot(
	ctx context.Context,
	sess *Session,
	trigger ProjectionTrigger,
) (bool, error) {
	if sess == nil {
		return false, nil
	}

	snapshot, err := s.buildSnapshot(sess, trigger)
	if err != nil {
		return false, err
	}

	if err := sess.Append(ctx, NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		return false, err
	}
	return true, nil
}

func (s *ProjectionSnapshotter) buildSnapshot(
	sess *Session,
	trigger ProjectionTrigger,
) (ProjectionSnapshot, error) {
	rebuilder := s.rebuilderOrDefault()

	sess.mu.Lock()
	defer sess.mu.Unlock()

	entries, err := rebuilder.rebuildEntriesLocked(sess)
	if err != nil {
		return ProjectionSnapshot{}, err
	}
	baseSnapshot, _, err := sess.latestDurableSnapshotLocked()
	if err != nil {
		return ProjectionSnapshot{}, err
	}

	cutoffEventID := ""
	if sess.activeLeafID != "" {
		cutoffEventID = sess.activeLeafID
	}

	snapshot := ProjectionSnapshot{
		Strategy:      string(trigger),
		CutoffEventID: cutoffEventID,
		Entries:       entries,
		Messages:      entriesToMessages(entries),
	}
	if len(baseSnapshot.ReadFiles) > 0 {
		snapshot.ReadFiles = append([]string(nil), baseSnapshot.ReadFiles...)
	}
	if len(baseSnapshot.ModifiedFiles) > 0 {
		snapshot.ModifiedFiles = append([]string(nil), baseSnapshot.ModifiedFiles...)
	}
	return snapshot, nil
}

func (s *ProjectionSnapshotter) shouldSnapshot(
	sess *Session,
) (ProjectionTrigger, bool, error) {
	if sess == nil {
		return "", false, nil
	}

	events := sess.Events()
	if len(events) == 0 {
		return "", false, nil
	}

	latestSnapshotIdx := -1
	for i := len(events) - 1; i >= 0; i-- {
		if isDurableSnapshotEvent(events[i].Type) {
			latestSnapshotIdx = i
			break
		}
	}

	now := s.now()
	if latestSnapshotIdx >= 0 {
		snapshotEvent := events[latestSnapshotIdx]
		deltaEvents := len(events) - latestSnapshotIdx - 1
		if deltaEvents == 0 {
			return "", false, nil
		}
		if s.maxEvents() > 0 && deltaEvents >= s.maxEvents() {
			return ProjectionTriggerCount, true, nil
		}
		if s.maxAge() > 0 && now.Sub(snapshotEvent.Timestamp) >= s.maxAge() {
			return ProjectionTriggerTime, true, nil
		}
		return "", false, nil
	}

	if s.maxEvents() > 0 && len(events) >= s.maxEvents() {
		return ProjectionTriggerCount, true, nil
	}
	if s.maxAge() > 0 && now.Sub(events[0].Timestamp) >= s.maxAge() {
		return ProjectionTriggerTime, true, nil
	}
	return "", false, nil
}

func (s *ProjectionSnapshotter) rebuilderOrDefault() *Rebuilder {
	if s != nil && s.Rebuilder != nil {
		return s.Rebuilder
	}
	return NewRebuilder()
}

func (s *ProjectionSnapshotter) now() time.Time {
	if s != nil && s.Now != nil {
		return s.Now().UTC()
	}
	return time.Now().UTC()
}

func (s *ProjectionSnapshotter) maxEvents() int {
	if s == nil {
		return 0
	}
	return s.MaxEvents
}

func (s *ProjectionSnapshotter) maxAge() time.Duration {
	if s == nil {
		return 0
	}
	return s.MaxAge
}

func isDurableSnapshotEvent(eventType EventType) bool {
	return eventType == CompactionTriggered || eventType == ProjectionSnapshotted
}

func entriesToMessages(entries []HistoryEntry) []llm.Message {
	messages := make([]llm.Message, 0, len(entries))
	for _, entry := range entries {
		messages = append(messages, entry.Message)
	}
	return messages
}
