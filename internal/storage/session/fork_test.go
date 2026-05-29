package session

import (
	"context"
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

func TestSessionFork_RegeneratesIDsAndAddsLineageMetadata(t *testing.T) {
	sess := New("parent")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "hello"},
		{Role: llm.RoleAssistant, Content: "hi"},
	} {
		if err := sess.Append(context.Background(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append: %v", err)
		}
	}

	parentEvents := sess.Events()
	forked := sess.Fork("child")
	childEvents := forked.Events()
	if len(childEvents) != len(parentEvents) {
		t.Fatalf("expected %d forked events, got %d", len(parentEvents), len(childEvents))
	}

	for i := range parentEvents {
		if childEvents[i].SessionID != "child" {
			t.Fatalf("child event session_id = %q, want child", childEvents[i].SessionID)
		}
		if childEvents[i].ID == parentEvents[i].ID {
			t.Fatalf("child event %d reused parent event ID %s", i, childEvents[i].ID)
		}

		origin, ok, err := childEvents[i].ForkOrigin()
		if err != nil {
			t.Fatalf("child event %d fork origin decode: %v", i, err)
		}
		if !ok {
			t.Fatalf("child event %d missing fork origin metadata: %#v", i, childEvents[i].Metadata)
		}
		if got := origin.SessionID; got != "parent" {
			t.Fatalf("fork origin session_id = %q, want parent", got)
		}
		if got := origin.EventID; got != parentEvents[i].ID.String() {
			t.Fatalf("fork origin event_id = %q, want %s", got, parentEvents[i].ID)
		}
	}
}

func TestSessionFork_RewritesCompactionSnapshotEventReferences(t *testing.T) {
	sess := New("parent")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "old user"},
		{Role: llm.RoleAssistant, Content: "old assistant"},
		{Role: llm.RoleUser, Content: "recent user"},
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append: %v", err)
		}
	}

	parentEvents := sess.Events()
	snapshot := CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: parentEvents[2].ID.String(),
		Entries: []HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
			{
				EventID: parentEvents[2].ID.String(),
				Message: llm.Message{Role: llm.RoleUser, Content: "recent user"},
			},
		},
	}
	if err := sess.Append(t.Context(), NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	forked := sess.Fork("child")
	childEvents := forked.Events()
	if len(childEvents) != 4 {
		t.Fatalf("expected 4 child events, got %d", len(childEvents))
	}

	childSnapshot, ok, err := childEvents[3].CompactionSnapshot()
	if err != nil {
		t.Fatalf("decode child compaction snapshot: %v", err)
	}
	if !ok {
		t.Fatal("expected child compaction snapshot")
	}
	if childSnapshot.CutoffEventID != childEvents[2].ID.String() {
		t.Fatalf(
			"child cutoff = %q, want %q",
			childSnapshot.CutoffEventID,
			childEvents[2].ID,
		)
	}
	if childSnapshot.Entries[1].EventID != childEvents[2].ID.String() {
		t.Fatalf(
			"child entry event_id = %q, want %q",
			childSnapshot.Entries[1].EventID,
			childEvents[2].ID,
		)
	}

	messages, err := forked.EffectiveMessages()
	if err != nil {
		t.Fatalf("effective messages: %v", err)
	}
	if len(messages) != 2 {
		t.Fatalf("expected 2 effective messages, got %d", len(messages))
	}
	if messages[1].Content != "recent user" {
		t.Fatalf("unexpected effective history: %#v", messages)
	}
}

func TestSessionFork_RewritesProjectionSnapshotEventReferences(t *testing.T) {
	sess := New("parent")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "old user"},
		{Role: llm.RoleAssistant, Content: "old assistant"},
		{Role: llm.RoleUser, Content: "recent user"},
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append: %v", err)
		}
	}

	parentEvents := sess.Events()
	snapshot := ProjectionSnapshot{
		Strategy:      "count",
		CutoffEventID: parentEvents[2].ID.String(),
		Entries: []HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
			{
				EventID: parentEvents[2].ID.String(),
				Message: llm.Message{Role: llm.RoleUser, Content: "recent user"},
			},
		},
	}
	if err := sess.Append(t.Context(), NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append projection snapshot: %v", err)
	}

	forked := sess.Fork("child")
	childEvents := forked.Events()
	if len(childEvents) != 4 {
		t.Fatalf("expected 4 child events, got %d", len(childEvents))
	}

	childSnapshot, ok, err := childEvents[3].ProjectionSnapshot()
	if err != nil {
		t.Fatalf("decode child projection snapshot: %v", err)
	}
	if !ok {
		t.Fatal("expected child projection snapshot")
	}
	if childSnapshot.CutoffEventID != childEvents[2].ID.String() {
		t.Fatalf(
			"child cutoff = %q, want %q",
			childSnapshot.CutoffEventID,
			childEvents[2].ID,
		)
	}
	if childSnapshot.Entries[1].EventID != childEvents[2].ID.String() {
		t.Fatalf(
			"child entry event_id = %q, want %q",
			childSnapshot.Entries[1].EventID,
			childEvents[2].ID,
		)
	}

	messages, err := forked.EffectiveMessages()
	if err != nil {
		t.Fatalf("effective messages: %v", err)
	}
	if len(messages) != 2 {
		t.Fatalf("expected 2 effective messages, got %d", len(messages))
	}
	if messages[0].Content != "<conversation_summary>\nsummary\n</conversation_summary>" {
		t.Fatalf("unexpected effective history: %#v", messages)
	}
	if messages[1].Content != "recent user" {
		t.Fatalf("unexpected effective history: %#v", messages)
	}
}
