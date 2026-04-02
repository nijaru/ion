package storage

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func TestCantoStoreAppendUpdatesRecentSession(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"

	first, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open first session: %v", err)
	}

	time.Sleep(1100 * time.Millisecond)

	second, err := store.OpenSession(ctx, cwd, "model-b", "main")
	if err != nil {
		t.Fatalf("open second session: %v", err)
	}

	recent, err := store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent session before append: %v", err)
	}
	if recent.ID != second.ID() {
		t.Fatalf("recent session before append = %q, want %q", recent.ID, second.ID())
	}

	time.Sleep(1100 * time.Millisecond)

	if err := first.Append(ctx, User{Content: "working"}); err != nil {
		t.Fatalf("append user: %v", err)
	}

	recent, err = store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent session after append: %v", err)
	}
	if recent.ID != first.ID() {
		t.Fatalf("recent session after append = %q, want %q", recent.ID, first.ID())
	}
	if recent.Title != "working" {
		t.Fatalf("recent title = %q, want %q", recent.Title, "working")
	}
	if recent.Summary != "working" || recent.LastPreview != "working" {
		t.Fatalf("recent summary = %q / %q, want %q", recent.Summary, recent.LastPreview, "working")
	}
}

func TestCantoStoreAppendReturnsPersistenceErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := store.db.Close(); err != nil {
		t.Fatalf("close db: %v", err)
	}

	if err := sess.Append(ctx, Status{Status: "still working"}); err == nil {
		t.Fatal("expected append to return an error when session metadata update fails")
	}
}

func TestCantoStoreEntriesMapToolMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}

	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})); err != nil {
		t.Fatalf("append user: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:      llm.RoleAssistant,
		Content:   "hi there",
		Reasoning: "reasoning",
	})); err != nil {
		t.Fatalf("append agent: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		Name:    "bash",
		Content: "tool output",
	})); err != nil {
		t.Fatalf("append tool: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("entries length = %d, want 3", len(entries))
	}
	if entries[0].Role != ionsession.User || entries[0].Content != "hello" {
		t.Fatalf("user entry = %#v", entries[0])
	}
	if entries[1].Role != ionsession.Agent || entries[1].Content != "hi there" || entries[1].Reasoning != "reasoning" {
		t.Fatalf("agent entry = %#v", entries[1])
	}
	if entries[2].Role != ionsession.Tool || entries[2].Title != "bash" || entries[2].Content != "tool output" {
		t.Fatalf("tool entry = %#v", entries[2])
	}
}

func TestCantoStoreEntriesMapSystemMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := sess.Append(ctx, System{Content: "Error: backend unavailable"}); err != nil {
		t.Fatalf("append system: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.System || entries[0].Content != "Error: backend unavailable" {
		t.Fatalf("system entry = %#v", entries[0])
	}
}

func TestCantoStoreAppendPersistsToolResultsIntoEffectiveHistory(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := sess.Append(ctx, ToolUse{
		Type: "tool_use",
		ID:   "tool-123",
		Name: "verify",
		Input: map[string]string{
			"args": "go test ./...",
		},
		TS: time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append tool use: %v", err)
	}
	if err := sess.Append(ctx, ToolResult{
		Type:      "tool_result",
		ToolUseID: "tool-123",
		Content:   "PASSED: 15/15 passed\nOK",
		TS:        time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append tool result: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.Tool {
		t.Fatalf("tool entry role = %q, want %q", entries[0].Role, ionsession.Tool)
	}
	if entries[0].Title != "verify" {
		t.Fatalf("tool entry title = %q, want %q", entries[0].Title, "verify")
	}
	if entries[0].Content != "PASSED: 15/15 passed\nOK" {
		t.Fatalf("tool entry content = %q", entries[0].Content)
	}
}

func TestCantoStoreEntriesUseEffectiveHistoryAfterCompaction(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}

	userEvent := csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "old question",
	})
	if err := cantoSess.Append(ctx, userEvent); err != nil {
		t.Fatalf("append user: %v", err)
	}
	agentEvent := csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "old answer",
	})
	if err := cantoSess.Append(ctx, agentEvent); err != nil {
		t.Fatalf("append agent: %v", err)
	}
	recentEvent := csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "recent answer",
	})
	if err := cantoSess.Append(ctx, recentEvent); err != nil {
		t.Fatalf("append recent agent: %v", err)
	}

	snapshot := csession.CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: recentEvent.ID.String(),
		Entries: []csession.HistoryEntry{
			{Message: llm.Message{Role: llm.RoleSystem, Content: "<conversation_summary>\nsummary\n</conversation_summary>"}},
			{EventID: recentEvent.ID.String(), Message: llm.Message{Role: llm.RoleAssistant, Content: "recent answer"}},
		},
	}
	if err := cantoSess.Append(ctx, csession.NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2", len(entries))
	}
	if entries[0].Role != ionsession.System || entries[0].Content != "<conversation_summary>\nsummary\n</conversation_summary>" {
		t.Fatalf("summary entry = %#v", entries[0])
	}
	if entries[1].Role != ionsession.Agent || entries[1].Content != "recent answer" {
		t.Fatalf("recent entry = %#v", entries[1])
	}
}
