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
	sess := NewSession(store, 64)

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
	sess := NewSession(store, 64)

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
	sess := NewSession(store, 64)

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
	sess := NewSession(store, 64)

	sess.AppendSessionInfo(ctx, "test session")
	sess.AppendMessage(ctx, NewUserText("hello", time.Now()))

	entries, _ := store.Entries(ctx)
	if len(entries) != 2 {
		t.Fatalf("expected 2 entries, got %d", len(entries))
	}
	if _, ok := entries[0].(*SessionInfoEntry); !ok {
		t.Fatalf("expected SessionInfoEntry, got %T", entries[0])
	}
	if _, ok := entries[1].(*MessageEntry); !ok {
		t.Fatalf("expected MessageEntry, got %T", entries[1])
	}
}

// REGRESSION: AppendLeafEntry must persist the leaf pointer so a linear
// session (one that never calls MoveTo) is reachable after reopening the
// store. Previously the leaf was only tracked in memory, so Branch() returned
// nil after restart and all history was silently lost.
func TestStoreLeafPersistedOnAppend(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	path := dir + "/session.db"

	s, err := NewSQLiteStore(path, "resume")
	if err != nil {
		t.Fatal(err)
	}
	sess := NewSession(s, 64)

	if _, err := sess.AppendMessage(ctx, NewUserText("hi", time.Now())); err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, &AssistantMessage{
		Content:    []Content{TextContent{Text: "hello"}},
		StopReason: StopReasonEndTurn,
		Timestamp:  time.Now(),
	}); err != nil {
		t.Fatal(err)
	}

	// Close and reopen from the same file.
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	s2, err := NewSQLiteStore(path, "resume")
	if err != nil {
		t.Fatal(err)
	}
	defer s2.Close()

	branch, err := s2.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 2 {
		t.Fatalf("expected 2 branch entries after reopen, got %d", len(branch))
	}
	if _, ok := branch[1].(*MessageEntry); !ok {
		t.Fatalf("expected MessageEntry after reopen, got %T", branch[1])
	}
}

// REGRESSION: CustomMessageEntry with non-text (image) content must round-trip.
// Previously the decode used a text-only heuristic that dropped images and
// corrupted any non-text block.
func TestStoreCustomMessageImageRoundTrip(t *testing.T) {
	ctx := context.Background()
	s := newTestStore(t)
	entry := &CustomMessageEntry{
		EntryBase:  EntryBase{ID: "cm1", Timestamp: time.Now()},
		CustomType: "image",
		Content:    []Content{ImageContent{Data: []byte("png-bytes"), MimeType: "image/png"}},
		Display:    "an image",
	}
	if _, err := s.Append(ctx, entry); err != nil {
		t.Fatal(err)
	}
	got, err := s.GetEntry(ctx, "cm1")
	if err != nil {
		t.Fatal(err)
	}
	ce, ok := got.(*CustomMessageEntry)
	if !ok {
		t.Fatalf("expected *CustomMessageEntry, got %T", got)
	}
	if len(ce.Content) != 1 {
		t.Fatalf("expected 1 content block, got %d", len(ce.Content))
	}
	img, ok := ce.Content[0].(ImageContent)
	if !ok {
		t.Fatalf("expected ImageContent, got %T", ce.Content[0])
	}
	if string(img.Data) != "png-bytes" || img.MimeType != "image/png" {
		t.Fatalf("image content corrupted: %+v", img)
	}
}

func TestSQLiteCatalogRoundTrip(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	s, err := NewSQLiteStore(dir, "catalog")
	if err != nil {
		t.Fatal(err)
	}
	if err := s.AddInput(ctx, "/repo", "first"); err != nil {
		t.Fatal(err)
	}
	if err := s.AddInput(ctx, "/repo", "second"); err != nil {
		t.Fatal(err)
	}
	if err := s.UpdateSession(ctx, SessionInfoEntry{
		EntryBase: EntryBase{ID: "session-1"},
		Workdir:   "/repo",
		Model:     "openrouter/test",
		Name:      "catalog test",
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}

	s, err = NewSQLiteStore(dir, "catalog")
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	inputs, err := s.GetInputs(ctx, "/repo", 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(inputs) != 2 || inputs[0] != "second" || inputs[1] != "first" {
		t.Fatalf("inputs = %#v, want newest-first history", inputs)
	}
	sessions, err := s.ListSessions(ctx, "/repo")
	if err != nil {
		t.Fatal(err)
	}
	if len(sessions) != 1 || sessions[0].ID() != "session-1" || sessions[0].Model != "openrouter/test" {
		t.Fatalf("sessions = %#v, want persisted catalog row", sessions)
	}
}

func TestSQLiteResumeSession(t *testing.T) {
	ctx := context.Background()
	s := newTestStore(t)
	entry := &MessageEntry{
		EntryBase: EntryBase{ID: "resume-entry", Timestamp: time.Now()},
		Message:   NewUserText("resume", time.Now()),
	}
	if _, err := s.Append(ctx, entry); err != nil {
		t.Fatal(err)
	}
	if err := s.ResumeSession(ctx, "resume-entry"); err != nil {
		t.Fatal(err)
	}
	if got := s.GetLeafID(); got != "resume-entry" {
		t.Fatalf("leaf = %q, want resume-entry", got)
	}
	if err := s.ResumeSession(ctx, "missing-entry"); err == nil {
		t.Fatal("expected missing resume entry to fail")
	}
	if got := s.GetLeafID(); got != "resume-entry" {
		t.Fatalf("leaf changed after failed resume: %q", got)
	}
}
