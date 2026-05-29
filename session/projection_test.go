package session

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
)

func TestProjectionSnapshotterSnapshotIfNeededUsesCountPolicy(t *testing.T) {
	sess := New("projection-count")
	snapshotter := &ProjectionSnapshotter{
		MaxEvents: 2,
		Rebuilder: NewRebuilder(),
	}

	first := llm.Message{Role: llm.RoleUser, Content: "one"}
	second := llm.Message{Role: llm.RoleAssistant, Content: "two"}
	for _, msg := range []llm.Message{first, second} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	ok, err := snapshotter.SnapshotIfNeeded(t.Context(), sess)
	if err != nil {
		t.Fatalf("SnapshotIfNeeded: %v", err)
	}
	if !ok {
		t.Fatal("expected snapshot to be appended")
	}

	events := sess.Events()
	if got := events[len(events)-1].Type; got != ProjectionSnapshotted {
		t.Fatalf("last event type = %q, want projection snapshot", got)
	}
	snapshot, ok, err := events[len(events)-1].ProjectionSnapshot()
	if err != nil {
		t.Fatalf("decode projection snapshot: %v", err)
	}
	if !ok {
		t.Fatal("expected projection snapshot payload")
	}
	if snapshot.Strategy != string(ProjectionTriggerCount) {
		t.Fatalf("snapshot strategy = %q, want %q", snapshot.Strategy, ProjectionTriggerCount)
	}
	if snapshot.CutoffEventID != events[len(events)-2].ID.String() {
		t.Fatalf(
			"snapshot cutoff = %q, want %q",
			snapshot.CutoffEventID,
			events[len(events)-2].ID,
		)
	}

	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "three",
	})); err != nil {
		t.Fatalf("append post-snapshot message: %v", err)
	}

	ok, err = snapshotter.SnapshotIfNeeded(t.Context(), sess)
	if err != nil {
		t.Fatalf("SnapshotIfNeeded after checkpoint: %v", err)
	}
	if ok {
		t.Fatal("expected checkpoint count to reset after projection snapshot")
	}
}

func TestProjectionSnapshotterSnapshotIfNeededUsesAgePolicy(t *testing.T) {
	sess := New("projection-age")
	now := time.Unix(1_000, 0).UTC()
	snapshotter := &ProjectionSnapshotter{
		MaxEvents: 100,
		MaxAge:    time.Minute,
		Now:       func() time.Time { return now.Add(2 * time.Minute) },
		Rebuilder: NewRebuilder(),
	}

	event := NewMessage(sess.ID(), llm.Message{Role: llm.RoleUser, Content: "one"})
	event.Timestamp = now
	if err := sess.Append(context.Background(), event); err != nil {
		t.Fatalf("append message: %v", err)
	}

	ok, err := snapshotter.SnapshotIfNeeded(t.Context(), sess)
	if err != nil {
		t.Fatalf("SnapshotIfNeeded: %v", err)
	}
	if !ok {
		t.Fatal("expected age policy to trigger a snapshot")
	}
}

func TestProjectionSnapshotterSnapshotAllowsInterleavedAppendBeforeSnapshotEvent(t *testing.T) {
	sess := New("projection-interleaved")
	snapshotter := NewProjectionSnapshotter()

	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "one",
	})); err != nil {
		t.Fatalf("append first message: %v", err)
	}

	snapshot, err := snapshotter.buildSnapshot(sess, ProjectionTriggerManual)
	if err != nil {
		t.Fatalf("build snapshot: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: "interleaved",
	})); err != nil {
		t.Fatalf("append interleaved message: %v", err)
	}
	if err := sess.Append(t.Context(), NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append snapshot: %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 2 {
		t.Fatalf("messages len = %d, want 2: %#v", len(messages), messages)
	}
	if messages[0].Content != "one" || messages[1].Content != "interleaved" {
		t.Fatalf("unexpected messages after interleaved snapshot append: %#v", messages)
	}
}
