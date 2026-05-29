package storage

import (
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	csession "github.com/nijaru/ion/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func TestLiveAndHistoryProjectionShareEntryShape(t *testing.T) {
	timestamp := time.Date(2026, 5, 25, 12, 0, 0, 0, time.FixedZone("test", -7*60*60))
	projector := New("/workspace")

	userLive, ok := EntryUser("hello", timestamp)
	if !ok {
		t.Fatal("live user projection rejected non-empty content")
	}
	userHistory, ok := projector.HistoryEntry(csession.HistoryEntry{
		Message: llm.Message{Role: llm.RoleUser, Content: "hello"},
	})
	if !ok {
		t.Fatal("history user projection rejected non-empty content")
	}
	userHistory = WithTimestamp(userHistory, timestamp)
	assertEntry(t, userLive, userHistory)

	agentLive, ok := EntryAgent("answer", "reason", timestamp)
	if !ok {
		t.Fatal("live agent projection rejected non-empty content")
	}
	agentHistory, ok := projector.HistoryEntry(csession.HistoryEntry{
		Message: llm.Message{
			Role:      llm.RoleAssistant,
			Content:   "answer",
			Reasoning: "reason",
		},
	})
	if !ok {
		t.Fatal("history agent projection rejected non-empty content")
	}
	agentHistory = WithTimestamp(agentHistory, timestamp)
	assertEntry(t, agentLive, agentHistory)

	toolLive, ok := Tool("Bash(go test ./...)", "ok", true, timestamp)
	if !ok {
		t.Fatal("live tool projection rejected tool output")
	}
	toolHistory, ok := projector.HistoryEntry(csession.HistoryEntry{
		Message: llm.Message{Role: llm.RoleTool, Content: "ok"},
		Tool: &csession.ToolHistory{
			Name:      "bash",
			Arguments: `{"command":"go test ./..."}`,
			IsError:   true,
		},
	})
	if !ok {
		t.Fatal("history tool projection rejected tool output")
	}
	toolHistory = WithTimestamp(toolHistory, timestamp)
	assertEntry(t, toolLive, toolHistory)
}

func TestProjectionDropsEmptyAssistantEntries(t *testing.T) {
	if entry, ok := EntryAgent("  ", "\n", time.Time{}); ok {
		t.Fatalf("empty live assistant projected as %#v", entry)
	}
	if entry, ok := New("").HistoryEntry(csession.HistoryEntry{
		Message: llm.Message{Role: llm.RoleAssistant, Content: "  "},
	}); ok {
		t.Fatalf("empty history assistant projected as %#v", entry)
	}
	got := Normalize([]ionsession.Entry{
		{Role: ionsession.User, Content: "hello"},
		{Role: ionsession.Agent, Content: "  "},
		{Role: ionsession.Agent, Reasoning: "kept"},
	})
	if len(got) != 2 {
		t.Fatalf("Normalize kept %d entries, want 2", len(got))
	}
	if got[1].Role != ionsession.Agent || got[1].Reasoning != "kept" {
		t.Fatalf("Normalize dropped reasoning-only assistant: %#v", got)
	}
}

func TestContextAndSnapshotProjection(t *testing.T) {
	projector := New("")
	contextEntry, ok := projector.HistoryEntry(csession.HistoryEntry{
		EventType:   csession.ContextAdded,
		ContextKind: csession.ContextKindSummary,
		Message:     llm.Message{Role: llm.RoleUser, Content: "summary"},
	})
	if !ok {
		t.Fatal("summary context was not projected")
	}
	if contextEntry.Role != ionsession.System || contextEntry.Content != "summary" {
		t.Fatalf("context entry = %#v", contextEntry)
	}

	entries := projector.SnapshotEntries(csession.CompactionSnapshot{
		Messages: []llm.Message{
			{Role: llm.RoleUser, Content: "user"},
			{Role: llm.RoleAssistant, Content: " "},
			{Role: llm.RoleAssistant, Reasoning: "why"},
		},
	})
	if len(entries) != 2 {
		t.Fatalf("snapshot projected %d entries, want 2: %#v", len(entries), entries)
	}
	if entries[0].Role != ionsession.User || entries[1].Reasoning != "why" {
		t.Fatalf("snapshot entries = %#v", entries)
	}
}

func assertEntry(t *testing.T, got, want ionsession.Entry) {
	t.Helper()
	if got != want {
		t.Fatalf("entry mismatch\ngot:  %#v\nwant: %#v", got, want)
	}
}
