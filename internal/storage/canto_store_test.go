package storage

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func appendCantoMessage(
	t *testing.T,
	store *cantoStore,
	ctx context.Context,
	sessionID string,
	msg llm.Message,
) {
	t.Helper()
	if err := store.canto.Save(ctx, csession.NewEvent(sessionID, csession.MessageAdded, msg)); err != nil {
		t.Fatalf("append canto message: %v", err)
	}
}

func appendLegacyCantoMessage(
	t *testing.T,
	store *cantoStore,
	ctx context.Context,
	sessionID string,
	msg llm.Message,
) {
	t.Helper()
	event := csession.NewEvent(sessionID, csession.MessageAdded, msg)
	if _, err := store.db.ExecContext(
		ctx,
		"INSERT INTO events (id, session_id, type, timestamp, data, metadata, cost) VALUES (?, ?, ?, ?, ?, ?, ?)",
		event.ID.String(),
		event.SessionID,
		string(event.Type),
		event.Timestamp.Format(time.RFC3339Nano),
		event.Data,
		nil,
		event.Cost,
	); err != nil {
		t.Fatalf("append legacy canto message: %v", err)
	}
}

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

	if err := first.Append(ctx, Status{Status: "working"}); err != nil {
		t.Fatalf("append status: %v", err)
	}

	recent, err = store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent session after append: %v", err)
	}
	if recent.ID != first.ID() {
		t.Fatalf("recent session after append = %q, want %q", recent.ID, first.ID())
	}
}

func TestCantoStoreListSessionsToleratesNullName(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	sess, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	if _, err := store.db.ExecContext(ctx, "UPDATE session_meta SET name = NULL WHERE id = ?", sess.ID()); err != nil {
		t.Fatalf("null session name: %v", err)
	}

	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("sessions = %d, want 1", len(sessions))
	}
	if sessions[0].Title != "" {
		t.Fatalf("title = %q, want empty", sessions[0].Title)
	}
}

func TestLazySessionDoesNotAppearUntilAppend(t *testing.T) {
	root := t.TempDir()
	store, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	lazy := NewLazySession(store, cwd, "model-a", "main")

	recent, err := store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent before append: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent before append = %#v, want nil", recent)
	}
	if IsMaterialized(lazy) {
		t.Fatal("lazy session materialized before append")
	}

	if err := lazy.Append(ctx, System{Content: "local notice"}); err != nil {
		t.Fatalf("append: %v", err)
	}
	if !IsMaterialized(lazy) {
		t.Fatal("lazy session did not materialize after append")
	}

	recent, err = store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent after append: %v", err)
	}
	if recent == nil || recent.ID != lazy.ID() {
		t.Fatalf("recent after append = %#v, want %q", recent, lazy.ID())
	}
}

func TestLazySessionSkipsEmptyAgentAppend(t *testing.T) {
	root := t.TempDir()
	store, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	lazy := NewLazySession(store, cwd, "model-a", "main")
	if err := lazy.Append(ctx, Agent{
		Type:    "agent",
		Content: []Block{},
		TS:      time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append empty agent: %v", err)
	}

	if IsMaterialized(lazy) {
		t.Fatal("lazy session materialized after empty agent append")
	}
	recent, err := store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent after empty append: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent after empty append = %#v, want nil", recent)
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

func TestCantoStoreEntriesSummarizeRoutineToolOutput(t *testing.T) {
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
		Role:    llm.RoleTool,
		Name:    "read",
		Content: strings.Join([]string{"line 1", "line 2", "line 3"}, "\n"),
	})); err != nil {
		t.Fatalf("append read: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
	if entries[0].Role != ionsession.User || entries[0].Content != "hello" {
		t.Fatalf("user entry = %#v", entries[0])
	}
	if entries[1].Role != ionsession.Tool || entries[1].Title != "read" || entries[1].Content != "... (3 lines)" {
		t.Fatalf("read entry = %#v", entries[1])
	}
}

func TestCantoStoreAppendSkipsEmptyAgentMessages(t *testing.T) {
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

	if err := sess.Append(ctx, Agent{
		Type:    "agent",
		Content: []Block{},
		TS:      time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append empty agent: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	for _, ev := range cantoSess.Events() {
		if ev.Type != csession.MessageAdded {
			continue
		}
		var msg llm.Message
		if err := ev.UnmarshalData(&msg); err != nil {
			t.Fatalf("unmarshal message: %v", err)
		}
		if msg.Role == llm.RoleAssistant {
			t.Fatalf("empty assistant message was appended: %#v", msg)
		}
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 0 {
		t.Fatalf("entries = %#v, want none", entries)
	}
}

func TestCantoStoreEntriesPreserveReasoningOnlyAgentMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	reasoning := "thinking through it"
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:      llm.RoleAssistant,
		Reasoning: reasoning,
	})

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.Agent || entries[0].Content != "" || entries[0].Reasoning != reasoning {
		t.Fatalf("reasoning-only agent entry = %#v", entries[0])
	}
}

func TestCantoStoreEntriesDropEmptyAgentMessages(t *testing.T) {
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

	appendMessage := func(role llm.Role, content string) {
		t.Helper()
		if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
			Role:    role,
			Content: content,
		})); err != nil {
			t.Fatalf("append %s message: %v", role, err)
		}
	}
	appendMessage(llm.RoleUser, "first")
	appendLegacyCantoMessage(t, store, ctx, sess.ID(), llm.Message{Role: llm.RoleAssistant})
	appendMessage(llm.RoleAssistant, "same")

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
	if entries[1].Role != ionsession.Agent || entries[1].Content != "same" {
		t.Fatalf("agent entry = %#v", entries[1])
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

func TestCantoStoreEntriesInterleaveSystemMessagesWithHistory(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "first turn",
	})
	if err := sess.Append(ctx, System{Content: "Canceled by user"}); err != nil {
		t.Fatalf("append system: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "second turn",
	})

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("entries length = %d, want 3", len(entries))
	}
	if entries[0].Role != ionsession.User || entries[0].Content != "first turn" {
		t.Fatalf("first entry = %#v", entries[0])
	}
	if entries[1].Role != ionsession.System || entries[1].Content != "Canceled by user" {
		t.Fatalf("second entry = %#v", entries[1])
	}
	if entries[2].Role != ionsession.User || entries[2].Content != "second turn" {
		t.Fatalf("third entry = %#v", entries[2])
	}
}

func TestCantoStoreRejectsModelVisibleAppends(t *testing.T) {
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

	content := "agent"
	cases := []any{
		User{Type: "user", Content: "user"},
		Agent{Type: "agent", Content: []Block{{Type: "text", Text: &content}}},
		ToolUse{Type: "tool_use", ID: "tool-123", Name: "verify"},
		ToolResult{Type: "tool_result", ToolUseID: "tool-123", Content: "ok"},
	}
	for _, event := range cases {
		err := sess.Append(ctx, event)
		if err == nil {
			t.Fatalf("append %T returned nil, want model-visible error", event)
		}
		if !strings.Contains(err.Error(), "cannot append model-visible") {
			t.Fatalf("append %T error = %q, want model-visible error", event, err)
		}
	}
}

func TestCantoStoreEntriesPreserveFullAgentContent(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	content := strings.Repeat("full assistant content ", 12)
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: content,
	})

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.Agent {
		t.Fatalf("agent entry role = %q, want %q", entries[0].Role, ionsession.Agent)
	}
	if entries[0].Content != content {
		t.Fatalf("agent content = %q, want full content %q", entries[0].Content, content)
	}
}

func TestCantoStoreEntriesPreserveToolResultErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-err",
		Name:    "bash",
		Content: "exit status 1",
	})
	if err := store.canto.Save(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "bash",
		ID:     "tool-err",
		Output: "exit status 1",
		Error:  "exit status 1",
	})); err != nil {
		t.Fatalf("save tool completed: %v", err)
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
	if !entries[0].IsError {
		t.Fatal("tool entry IsError = false, want true")
	}
}

func TestCantoStoreEntriesDoNotCompactRoutineToolErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	fullError := "permission denied\nError: exit status 1"
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-list-error",
		Name:    "list",
		Content: fullError,
	})
	if err := store.canto.Save(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "list",
		ID:     "tool-list-error",
		Output: fullError,
		Error:  fullError,
	})); err != nil {
		t.Fatalf("save tool completed: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Content != fullError {
		t.Fatalf("tool error content = %q, want full error", entries[0].Content)
	}
	if !entries[0].IsError {
		t.Fatal("tool entry IsError = false, want true")
	}
}

func TestCantoStoreEntriesPreserveCantoToolCompletedErrors(t *testing.T) {
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

	if err := store.canto.Save(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-err",
		Name:    "bash",
		Content: "exit status 1",
	})); err != nil {
		t.Fatalf("save tool message: %v", err)
	}
	if err := store.canto.Save(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "bash",
		ID:     "tool-err",
		Output: "exit status 1",
		Error:  "exit status 1",
	})); err != nil {
		t.Fatalf("save tool completed: %v", err)
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
	if !entries[0].IsError {
		t.Fatal("tool entry IsError = false, want true")
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
