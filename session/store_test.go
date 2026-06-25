package session

import (
	"context"
	"testing"
	"time"
)

// newTestStore creates an in-memory SQLiteStore for testing.
func newTestStore(t *testing.T) *SQLiteStore {
	t.Helper()
	s, err := NewSQLiteStore(":memory:", "test-session")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })
	return s
}

// --- Store contract tests ---

// INVARIANT: Append + GetEntry round-trips preserve the entry.
func TestStoreAppendGetEntry(t *testing.T) {
	ctx := context.Background()
	s := newTestStore(t)

	msg := NewUserText("hello", time.Now())
	entry := &MessageEntry{
		EntryBase: EntryBase{ID: "e1", ParentID: "", Timestamp: msg.Timestamp},
		Message:   msg,
	}
	_, err := s.Append(ctx, entry)
		if err != nil {
		t.Fatal(err)
	}
	got, err := s.GetEntry(ctx, "e1")
	if err != nil {
		t.Fatal(err)
	}
	me, ok := got.(*MessageEntry)
	if !ok {
		t.Fatalf("expected *MessageEntry, got %T", got)
	}
	um, ok := me.Message.(*UserMessage)
	if !ok {
		t.Fatalf("expected *UserMessage, got %T", me.Message)
	}
	if len(um.Content) != 1 {
		t.Fatalf("expected 1 content block, got %d", len(um.Content))
	}
	tc, ok := um.Content[0].(TextContent)
	if !ok {
		t.Fatalf("expected TextContent, got %T", um.Content[0])
	}
	if tc.Text != "hello" {
		t.Fatalf("expected text %q, got %q", "hello", tc.Text)
	}
}

// INVARIANT: Branch returns entries root-to-leaf in order.
func TestStoreBranchOrder(t *testing.T) {
	ctx := context.Background()
	s := newTestStore(t)

	e1 := &MessageEntry{EntryBase: EntryBase{ID: "e1", ParentID: "", Timestamp: time.Now()}, Message: NewUserText("a", time.Now())}
	e2 := &MessageEntry{EntryBase: EntryBase{ID: "e2", ParentID: "e1", Timestamp: time.Now()}, Message: &AssistantMessage{StopReason: StopReasonEndTurn, Timestamp: time.Now()}}
	e3 := &MessageEntry{EntryBase: EntryBase{ID: "e3", ParentID: "e2", Timestamp: time.Now()}, Message: NewUserText("b", time.Now())}

	for _, e := range []Entry{e1, e2, e3} {
		if _, err := s.Append(ctx, e); err != nil {
			t.Fatal(err)
		}
	}
	s.SetLeafID("e3")

	branch, err := s.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 3 {
		t.Fatalf("expected 3 entries, got %d", len(branch))
	}
	if branch[0].ID() != "e1" || branch[1].ID() != "e2" || branch[2].ID() != "e3" {
		t.Fatalf("wrong order: %s, %s, %s", branch[0].ID(), branch[1].ID(), branch[2].ID())
	}
}

// INVARIANT: leaf pointer tracks the latest append.
func TestStoreLeafPointer(t *testing.T) {
	ctx := context.Background()
	s := newTestStore(t)

	if s.GetLeafID() != "" {
		t.Fatalf("expected empty leaf, got %q", s.GetLeafID())
	}
	e1 := &MessageEntry{EntryBase: EntryBase{ID: "e1", ParentID: "", Timestamp: time.Now()}, Message: NewUserText("x", time.Now())}
	s.Append(ctx, e1)
	s.SetLeafID("e1")
	if s.GetLeafID() != "e1" {
		t.Fatalf("expected leaf %q, got %q", "e1", s.GetLeafID())
	}
}

// INVARIANT: all entry types round-trip through the store.
func TestStoreAllEntryTypes(t *testing.T) {
	ctx := context.Background()
	s := newTestStore(t)
	ts := time.Now()

	entries := []Entry{
		&MessageEntry{EntryBase: EntryBase{ID: "m1", Timestamp: ts}, Message: NewUserText("hi", ts)},
		&ModelChangeEntry{EntryBase: EntryBase{ID: "mc1", Timestamp: ts}, Provider: "anthropic", ModelID: "claude"},
		&ThinkingChangeEntry{EntryBase: EntryBase{ID: "tc1", Timestamp: ts}, Level: ThinkingHigh},
		&ToolsChangeEntry{EntryBase: EntryBase{ID: "tl1", Timestamp: ts}, ActiveTools: []string{"bash", "edit"}},
		&CompactionEntry{EntryBase: EntryBase{ID: "c1", Timestamp: ts}, Summary: "summarized", FirstKeptID: "m1", TokensBefore: 1000},
		&BranchSummaryEntry{EntryBase: EntryBase{ID: "bs1", Timestamp: ts}, Summary: "branched"},
		&LabelEntry{EntryBase: EntryBase{ID: "l1", Timestamp: ts}, TargetID: "m1", Label: "important"},
		&SessionInfoEntry{EntryBase: EntryBase{ID: "si1", Timestamp: ts}, Name: "my session"},
		&CustomEntry{EntryBase: EntryBase{ID: "cu1", Timestamp: ts}, Type: "status", Data: []byte(`{"ok":true}`)},
	}
	for _, e := range entries {
		if _, err := s.Append(ctx, e); err != nil {
			t.Fatalf("append %T(%s): %v", e, e.ID(), err)
		}
	}
	got, err := s.Entries(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != len(entries) {
		t.Fatalf("expected %d entries, got %d", len(entries), len(got))
	}
}

// --- Session contract tests ---

// INVARIANT: AppendMessage creates a MessageEntry and advances the leaf.
func TestSessionAppendMessage(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := NewSession(store)

	id, err := sess.AppendMessage(ctx, NewUserText("hello", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if id == "" {
		t.Fatal("expected non-empty ID")
	}
	if store.GetLeafID() != id {
		t.Fatalf("leaf not advanced: want %q, got %q", id, store.GetLeafID())
	}
}

// INVARIANT: BuildContext reconstructs []Message from the branch.
func TestSessionBuildContext(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := NewSession(store)

	sess.AppendMessage(ctx, NewUserText("hi", time.Now()))
	sess.AppendMessage(ctx, &AssistantMessage{StopReason: StopReasonEndTurn, Timestamp: time.Now()})

	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Messages) != 2 {
		t.Fatalf("expected 2 messages, got %d", len(snap.Messages))
	}
	if _, ok := snap.Messages[0].(*UserMessage); !ok {
		t.Fatalf("expected UserMessage, got %T", snap.Messages[0])
	}
	if _, ok := snap.Messages[1].(*AssistantMessage); !ok {
		t.Fatalf("expected AssistantMessage, got %T", snap.Messages[1])
	}
}

// INVARIANT: Usage accumulates from AssistantMessages.
func TestSessionUsage(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := NewSession(store)

	sess.AppendMessage(ctx, &AssistantMessage{
		Usage: Usage{Input: 100, Output: 50, Cost: Cost{Total: 0.01}},
		StopReason: StopReasonEndTurn, Timestamp: time.Now(),
	})
	sess.AppendMessage(ctx, &AssistantMessage{
		Usage: Usage{Input: 200, Output: 80, Cost: Cost{Total: 0.03}},
		StopReason: StopReasonEndTurn, Timestamp: time.Now(),
	})

	u, err := sess.Usage(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if u.Input != 300 || u.Output != 130 {
		t.Fatalf("usage: input=%d output=%d, want 300/130", u.Input, u.Output)
	}
	if u.Cost.Total != 0.04 {
		t.Fatalf("cost: %f, want 0.04", u.Cost.Total)
	}
}

// INVARIANT: typed append methods create the correct entry kinds.
func TestSessionTypedAppends(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := NewSession(store)

	sess.AppendModelChange(ctx, "openai", "gpt-4")
	sess.AppendThinkingChange(ctx, ThinkingHigh)
	sess.AppendToolsChange(ctx, []string{"bash"})
	sess.AppendSessionInfo(ctx, "test session")

	entries, _ := store.Entries(ctx)
	if len(entries) != 4 {
		t.Fatalf("expected 4 entries, got %d", len(entries))
	}
	if _, ok := entries[0].(*ModelChangeEntry); !ok {
		t.Fatalf("expected ModelChangeEntry, got %T", entries[0])
	}
	if _, ok := entries[1].(*ThinkingChangeEntry); !ok {
		t.Fatalf("expected ThinkingChangeEntry, got %T", entries[1])
	}
	if _, ok := entries[2].(*ToolsChangeEntry); !ok {
		t.Fatalf("expected ToolsChangeEntry, got %T", entries[2])
	}
	if _, ok := entries[3].(*SessionInfoEntry); !ok {
		t.Fatalf("expected SessionInfoEntry, got %T", entries[3])
	}
}

// INVARIANT: MoveTo navigates to a new leaf.
func TestSessionMoveTo(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := NewSession(store)

	sess.AppendMessage(ctx, NewUserText("a", time.Now()))
	id2, _ := sess.AppendMessage(ctx, NewUserText("b", time.Now()))
	sess.AppendMessage(ctx, NewUserText("c", time.Now()))

	// Move back to id2.
	if _, err := sess.MoveTo(ctx, id2, nil); err != nil {
		t.Fatal(err)
	}
	if store.GetLeafID() != id2 {
		t.Fatalf("leaf: want %q, got %q", id2, store.GetLeafID())
	}
}
