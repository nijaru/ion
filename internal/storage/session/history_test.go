package session

import (
	"context"
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

func TestSessionEffectiveMessagesUsesLatestCompactionSnapshot(t *testing.T) {
	sess := New("compacted-session")
	history := []llm.Message{
		{Role: llm.RoleUser, Content: "old user"},
		{Role: llm.RoleAssistant, Content: "old assistant"},
		{Role: llm.RoleUser, Content: "recent"},
	}
	for _, msg := range history {
		if err := sess.Append(context.Background(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append history: %v", err)
		}
	}

	events := sess.Events()
	snapshot := CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: events[len(events)-1].ID.String(),
		Messages: []llm.Message{
			{
				Role:    llm.RoleSystem,
				Content: "<conversation_summary>\nsummary\n</conversation_summary>",
			},
			history[len(history)-1],
		},
	}
	if err := sess.Append(context.Background(), NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	after := llm.Message{Role: llm.RoleAssistant, Content: "after"}
	if err := sess.Append(context.Background(), NewMessage(sess.ID(), after)); err != nil {
		t.Fatalf("append post-compaction message: %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("effective messages: %v", err)
	}
	if len(messages) != 3 {
		t.Fatalf("expected 3 effective messages, got %d", len(messages))
	}
	if messages[0].Content != "<conversation_summary>\nsummary\n</conversation_summary>" {
		t.Fatalf("unexpected summary message: %q", messages[0].Content)
	}
	if messages[1].Content != "recent" || messages[2].Content != "after" {
		t.Fatalf("unexpected effective history: %#v", messages)
	}
}

func TestEventCompactionSnapshotDecode(t *testing.T) {
	event := NewCompactionEvent("sess", CompactionSnapshot{
		Strategy:      "offload",
		CutoffEventID: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "summary"},
		},
	})

	snapshot, ok, err := event.CompactionSnapshot()
	if err != nil {
		t.Fatalf("decode compaction snapshot: %v", err)
	}
	if !ok {
		t.Fatal("expected compaction snapshot metadata")
	}
	if snapshot.Strategy != "offload" || snapshot.CutoffEventID == "" {
		t.Fatalf("unexpected compaction snapshot: %#v", snapshot)
	}
}

func TestEventProjectionSnapshotDecode(t *testing.T) {
	event := NewProjectionSnapshot("sess", ProjectionSnapshot{
		Strategy:      "count",
		CutoffEventID: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "projection"},
		},
	})

	snapshot, ok, err := event.ProjectionSnapshot()
	if err != nil {
		t.Fatalf("decode projection snapshot: %v", err)
	}
	if !ok {
		t.Fatal("expected projection snapshot metadata")
	}
	if snapshot.Strategy != "count" || snapshot.CutoffEventID == "" {
		t.Fatalf("unexpected projection snapshot: %#v", snapshot)
	}
}
