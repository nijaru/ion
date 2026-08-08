package session

import (
	"context"
	"database/sql"
	"errors"
	"os"
	"path/filepath"
	"strings"
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

	e1 := &MessageEntry{
		EntryBase: EntryBase{ID: "e1", ParentID: "", Timestamp: time.Now()},
		Message:   NewUserText("a", time.Now()),
	}
	e2 := &MessageEntry{
		EntryBase: EntryBase{ID: "e2", ParentID: "e1", Timestamp: time.Now()},
		Message:   &AssistantMessage{StopReason: StopReasonEndTurn, Timestamp: time.Now()},
	}
	e3 := &MessageEntry{
		EntryBase: EntryBase{ID: "e3", ParentID: "e2", Timestamp: time.Now()},
		Message:   NewUserText("b", time.Now()),
	}

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

	captured, err := s.BranchAt(ctx, "e2")
	if err != nil {
		t.Fatal(err)
	}
	if err := s.SetLeafID("e3"); err != nil {
		t.Fatal(err)
	}
	if len(captured) != 2 || captured[0].ID() != "e1" || captured[1].ID() != "e2" {
		t.Fatalf("captured branch = %v, want [e1 e2]", entryIDs(captured))
	}
}

func TestStoreBranchMissingLeafReturnsNoRows(t *testing.T) {
	s := newTestStore(t)
	if err := s.SetLeafID("missing"); !errors.Is(err, sql.ErrNoRows) {
		t.Fatalf("SetLeafID(missing) = %v, want sql.ErrNoRows", err)
	}
	if got := s.GetLeafID(); got != "" {
		t.Fatalf("leaf changed after rejected update: %q", got)
	}
}

// INVARIANT: leaf pointer tracks the latest append.
func TestStoreLeafPointer(t *testing.T) {
	ctx := context.Background()
	s := newTestStore(t)

	if s.GetLeafID() != "" {
		t.Fatalf("expected empty leaf, got %q", s.GetLeafID())
	}
	e1 := &MessageEntry{
		EntryBase: EntryBase{ID: "e1", ParentID: "", Timestamp: time.Now()},
		Message:   NewUserText("x", time.Now()),
	}
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
		&CompactionEntry{
			EntryBase:    EntryBase{ID: "c1", Timestamp: ts},
			Summary:      "summarized",
			FirstKeptID:  "m1",
			TokensBefore: 1000,
		},
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
		Usage:      Usage{Input: 100, Output: 50, Cost: Cost{Total: 0.01}},
		StopReason: StopReasonEndTurn, Timestamp: time.Now(),
	})
	sess.AppendMessage(ctx, &AssistantMessage{
		Usage:      Usage{Input: 200, Output: 80, Cost: Cost{Total: 0.03}},
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

func TestSQLiteMigratesUnversionedStoreTransactionally(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "legacy.db")
	raw, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	legacySchema := `
		CREATE TABLE entries (
			id TEXT PRIMARY KEY,
			parent_id TEXT NOT NULL DEFAULT '',
			type TEXT NOT NULL,
			timestamp INTEGER NOT NULL,
			payload BLOB NOT NULL DEFAULT '{}'
		);
		CREATE TABLE session_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);`
	if _, err := raw.ExecContext(ctx, legacySchema); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	legacyEntry := &MessageEntry{
		EntryBase: EntryBase{ID: "legacy-entry", Timestamp: time.Now()},
		Message:   NewUserText("legacy", time.Now()),
	}
	typ, payload, err := encodeEntry(legacyEntry)
	if err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if _, err := raw.ExecContext(ctx,
		"INSERT INTO entries(id,parent_id,type,timestamp,payload) VALUES(?,?,?,?,?)",
		legacyEntry.ID(), legacyEntry.ParentID(), typ, legacyEntry.When().UnixMilli(), payload); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if _, err := raw.ExecContext(ctx, `
		INSERT INTO session_meta(key,value) VALUES
			('session_id','legacy-session'), ('leaf_id','legacy-entry')`); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if err := raw.Close(); err != nil {
		t.Fatal(err)
	}

	store, err := NewSQLiteStore(path, "ignored-request-id")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	var version int
	if err := store.db.QueryRowContext(ctx, "PRAGMA user_version").Scan(&version); err != nil {
		t.Fatal(err)
	}
	if version != currentSchemaVersion {
		t.Fatalf("schema version = %d, want %d", version, currentSchemaVersion)
	}
	if _, err := os.Stat(path + ".pre-migration-v0"); err != nil {
		t.Fatalf("migration backup missing: %v", err)
	}
	if got := store.Meta().ID; got != "legacy-session" {
		t.Fatalf("session ID = %q, want durable legacy identity", got)
	}
	if got := store.GetLeafID(); got != "legacy-entry" {
		t.Fatalf("leaf = %q, want legacy-entry", got)
	}
	if _, err := NewSession(store, 0).AppendMessage(ctx, NewUserText("after migration", time.Now())); err != nil {
		t.Fatal(err)
	}
	branch, err := store.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 2 || branch[0].ID() != "legacy-entry" {
		t.Fatalf("migrated branch = %v, want legacy entry plus new entry", entryIDs(branch))
	}
}

func TestSQLiteMigratesProcessGroupStorageToProcessIdentity(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "process-identity.db")
	raw, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	legacySchema := strings.ReplaceAll(Schema, "process_identity", "process_group_id")
	if _, err := raw.ExecContext(ctx, legacySchema); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if _, err := raw.ExecContext(ctx, "PRAGMA user_version = 9"); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if _, err := raw.ExecContext(ctx, `
		INSERT INTO actions(action_id, invocation_id, tool_name, operation, arguments, fingerprint, state, process_group_id, prepared_at)
		VALUES('legacy-action', 'legacy-call', 'bash', 'run', '{"command":"true"}', 'legacy-fingerprint', 'started', 'legacy-process', 1)`); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if err := raw.Close(); err != nil {
		t.Fatal(err)
	}

	store, err := NewSQLiteStore(path, "ignored")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	action, err := store.GetAction(ctx, "legacy-action")
	if err != nil {
		t.Fatal(err)
	}
	if action.ProcessIdentity != "legacy-process" {
		t.Fatalf("migrated process identity = %q", action.ProcessIdentity)
	}
	var oldColumns, newColumns int
	rows, err := store.db.QueryContext(ctx, "PRAGMA table_info(actions)")
	if err != nil {
		t.Fatal(err)
	}
	for rows.Next() {
		var cid, notNull, pk int
		var name, typ string
		var defaultValue sql.NullString
		if err := rows.Scan(&cid, &name, &typ, &notNull, &defaultValue, &pk); err != nil {
			rows.Close()
			t.Fatal(err)
		}
		switch name {
		case "process_group_id":
			oldColumns++
		case "process_identity":
			newColumns++
		}
	}
	if err := rows.Err(); err != nil {
		rows.Close()
		t.Fatal(err)
	}
	rows.Close()
	if oldColumns != 0 || newColumns != 1 {
		t.Fatalf("process identity columns = old %d, new %d", oldColumns, newColumns)
	}
}

func TestSQLiteRejectsConflictingDualProcessIdentityColumns(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "conflicting-process-identity.db")
	raw, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	legacySchema := strings.ReplaceAll(Schema, "process_identity", "process_group_id")
	if _, err := raw.ExecContext(ctx, legacySchema+`
		ALTER TABLE actions ADD COLUMN process_identity TEXT NOT NULL DEFAULT '';`); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if _, err := raw.ExecContext(ctx, "PRAGMA user_version = 9"); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if _, err := raw.ExecContext(ctx, `
		INSERT INTO actions(action_id, invocation_id, tool_name, operation, arguments, fingerprint, state, process_group_id, process_identity, prepared_at)
		VALUES('conflicting-action', 'conflicting-call', 'bash', 'run', '{"command":"true"}', 'conflicting-fingerprint', 'started', 'legacy-process', 'different-process', 1)`); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if err := raw.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := NewSQLiteStore(
		path,
		"ignored",
	); err == nil ||
		!strings.Contains(err.Error(), "conflicting process identity columns") {
		t.Fatalf("conflicting dual-column store error = %v", err)
	}
}

func TestSQLiteStoreEnforcesSingleWriterLock(t *testing.T) {
	path := filepath.Join(t.TempDir(), "session.db")
	first, err := NewSQLiteStore(path, "first")
	if err != nil {
		t.Fatal(err)
	}
	second, err := NewSQLiteStore(path, "second")
	if !errors.Is(err, ErrSessionBusy) {
		if second != nil {
			_ = second.Close()
		}
		t.Fatalf("second store error = %v, want ErrSessionBusy", err)
	}
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}
	reopened, err := NewSQLiteStore(path, "third")
	if err != nil {
		t.Fatal(err)
	}
	if err := reopened.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestSQLiteStoreRejectsCorruptLeafOnOpen(t *testing.T) {
	path := filepath.Join(t.TempDir(), "session.db")
	store, err := NewSQLiteStore(path, "corrupt-leaf")
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
	raw, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := raw.Exec(
		"INSERT INTO session_meta(key,value) VALUES('leaf_id','missing') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
	); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if err := raw.Close(); err != nil {
		t.Fatal(err)
	}
	opened, err := NewSQLiteStore(path, "ignored")
	if opened != nil {
		_ = opened.Close()
	}
	if !errors.Is(err, ErrCorruptSession) {
		t.Fatalf("corrupt leaf open error = %v, want ErrCorruptSession", err)
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

func TestSessionIdentityIsStableAcrossLeafMovementAndReopen(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "session.db")
	store, err := NewSQLiteStore(path, "requested-id")
	if err != nil {
		t.Fatal(err)
	}
	sess := NewSession(store, 0)
	stableID := sess.ID()
	if stableID == "" {
		t.Fatal("session identity is empty")
	}
	messageID, err := sess.AppendMessage(ctx, NewUserText("hello", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetLeafID(messageID); err != nil {
		t.Fatal(err)
	}
	if got := sess.ID(); got != stableID {
		t.Fatalf("session ID changed with leaf: got %q, want %q", got, stableID)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := NewSQLiteStore(path, "different-request")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	if got := reopened.Meta().ID; got != stableID {
		t.Fatalf("reopened session ID = %q, want %q", got, stableID)
	}
}

func TestTurnEntriesBecomeVisibleOnlyAfterCommit(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "session.db")
	store, err := NewSQLiteStore(path, "turn-session")
	if err != nil {
		t.Fatal(err)
	}
	sess := NewSession(store, 0)
	baseID, err := sess.AppendMessage(ctx, NewUserText("base", time.Now()))
	if err != nil {
		t.Fatal(err)
	}

	images := []ImageContent{{Data: []byte("prompt-image"), MimeType: "image/png"}}
	record, err := store.BeginTurn(ctx, "turn-commit", "draft", images, "context-base")
	if err != nil {
		t.Fatal(err)
	}
	if record.State != TurnStarted || record.Sequence == 0 || record.LeafID != baseID || record.Input != "draft" ||
		len(record.InputImages) != 1 ||
		string(record.InputImages[0].Data) != "prompt-image" ||
		record.ContextID != "context-base" {
		t.Fatalf("unexpected begin record: %+v", record)
	}
	gotRecord, err := store.GetTurn(ctx, record.ID)
	if err != nil || gotRecord.State != TurnStarted {
		t.Fatalf("GetTurn(started) = (%+v, %v), want started record", gotRecord, err)
	}
	latestRecord, err := store.LatestTurn(ctx)
	if err != nil || latestRecord.ID != record.ID {
		t.Fatalf("LatestTurn(started) = (%+v, %v), want %q", latestRecord, err, record.ID)
	}
	draft := &MessageEntry{
		EntryBase: EntryBase{ID: "draft-commit", ParentID: baseID, Timestamp: time.Now()},
		Message:   NewUserText("draft", time.Now()),
	}
	if _, err := store.AppendTurnEntry(ctx, record.ID, draft); err != nil {
		t.Fatal(err)
	}
	branch, err := sess.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 1 || branch[0].ID() != baseID {
		t.Fatalf("uncommitted branch = %v, want only %q", entryIDs(branch), baseID)
	}
	turnBranch, err := store.TurnBranch(ctx, record.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(turnBranch) != 2 || turnBranch[1].ID() != draft.ID() {
		t.Fatalf("active turn branch = %v, want staged draft", entryIDs(turnBranch))
	}
	turnContext, err := ProjectContext(turnBranch)
	if err != nil {
		t.Fatal(err)
	}
	if len(turnContext.Messages) != 2 {
		t.Fatalf("active turn context messages = %d, want 2", len(turnContext.Messages))
	}
	if err := store.CommitTurn(ctx, record.ID); err != nil {
		t.Fatal(err)
	}
	branch, err = sess.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 2 || branch[1].ID() != draft.ID() {
		t.Fatalf("committed branch = %v, want draft appended", entryIDs(branch))
	}
	gotRecord, err = store.GetTurn(ctx, record.ID)
	if err != nil || gotRecord.State != TurnCommitted || gotRecord.EndedAt.IsZero() {
		t.Fatalf("GetTurn(committed) = (%+v, %v), want ended committed record", gotRecord, err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestInterruptedTurnIsRetainedButExcludedFromReplay(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "session.db")
	store, err := NewSQLiteStore(path, "interrupted-session")
	if err != nil {
		t.Fatal(err)
	}
	sess := NewSession(store, 0)
	baseID, err := sess.AppendMessage(ctx, NewUserText("base", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	record, err := store.BeginTurn(ctx, "turn-interrupted", "draft", nil, "context-base")
	if err != nil {
		t.Fatal(err)
	}
	draft := &MessageEntry{
		EntryBase: EntryBase{ID: "draft-interrupted", ParentID: baseID, Timestamp: time.Now()},
		Message:   NewUserText("must not replay", time.Now()),
	}
	if _, err := store.AppendTurnEntry(ctx, record.ID, draft); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := NewSQLiteStore(path, "interrupted-session")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	replayed := NewSession(reopened, 0)
	gotRecord, err := reopened.GetTurn(ctx, record.ID)
	if err != nil || gotRecord.State != TurnInterrupted || gotRecord.EndedAt.IsZero() {
		t.Fatalf("GetTurn(interrupted) = (%+v, %v), want ended interrupted record", gotRecord, err)
	}
	turns, err := reopened.InterruptedTurns(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(turns) != 1 || turns[0].ID != record.ID || turns[0].State != TurnInterrupted {
		t.Fatalf("interrupted turns = %+v", turns)
	}
	branch, err := replayed.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 1 || branch[0].ID() != baseID {
		t.Fatalf("replayed branch = %v, want only committed base", entryIDs(branch))
	}
	if err := reopened.AbortTurn(ctx, record.ID, "user discarded interrupted turn"); err != nil {
		t.Fatal(err)
	}
	turns, err = reopened.InterruptedTurns(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(turns) != 0 {
		t.Fatalf("interrupted turns after explicit abort = %+v", turns)
	}
}

func TestMoveToPersistsBranchSummaryUsage(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := NewSession(store, 0)
	first, err := sess.AppendMessage(ctx, NewUserText("first", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, NewUserText("second", time.Now())); err != nil {
		t.Fatal(err)
	}

	usage := Usage{Input: 11, Output: 7, TotalTokens: 18, Cost: Cost{Total: 0.25}}
	summaryID, err := sess.MoveTo(ctx, first, &BranchSummaryData{Summary: "branch", Usage: usage})
	if err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	entry, err := store.GetEntry(ctx, summaryID)
	if err != nil {
		t.Fatalf("GetEntry(summary): %v", err)
	}
	summary, ok := entry.(*BranchSummaryEntry)
	if !ok {
		t.Fatalf("summary entry = %T, want *BranchSummaryEntry", entry)
	}
	if summary.Usage != usage {
		t.Fatalf("summary usage = %#v, want %#v", summary.Usage, usage)
	}
}

func TestMoveToIsAtomicAndRejectsMissingTarget(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := NewSession(store, 0)
	first, err := sess.AppendMessage(ctx, NewUserText("first", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := sess.MoveTo(ctx, "missing", nil); !errors.Is(err, sql.ErrNoRows) {
		t.Fatalf("MoveTo(missing) = %v, want sql.ErrNoRows", err)
	}
	if got := store.GetLeafID(); got != first {
		t.Fatalf("leaf after rejected move = %q, want %q", got, first)
	}
}

func entryIDs(entries []Entry) []string {
	ids := make([]string, len(entries))
	for i, entry := range entries {
		ids[i] = entry.ID()
	}
	return ids
}
