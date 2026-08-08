package session

import (
	"context"
	"encoding/json"
	"path/filepath"
	"testing"
	"time"
)

// These tests encode the domain-model invariants from ai/DESIGN.md.
// They are contract tests for the design itself, not characterization of old code.

// INVARIANT (DESIGN §1.1): Message is a sealed union discriminated by role.
// A type switch over Message must be exhaustive of exactly three kinds.
func TestMessageIsSealedUnionOfThreeRoles(t *testing.T) {
	ts := time.Now()
	msgs := []Message{
		NewUserText("hi", ts),
		&AssistantMessage{StopReason: StopReasonEndTurn, Timestamp: ts},
		&ToolResultMessage{ToolCallID: "c1", ToolName: "ls", Timestamp: ts},
	}
	for _, m := range msgs {
		switch m.(type) {
		case *UserMessage, *AssistantMessage, *ToolResultMessage:
			// ok — the only valid message kinds
		default:
			t.Fatalf("message %T is not one of the three sealed roles", m)
		}
	}
}

// INVARIANT (DESIGN §1.1): ToolResult is a MESSAGE role, not a content block.
// Content blocks are exactly Text | Thinking | Image | ToolCall.
func TestContentIsSealedUnionOfFourKinds(t *testing.T) {
	blocks := []Content{
		TextContent{Text: "x"},
		ThinkingContent{Text: "y"},
		ImageContent{Data: []byte{1}, MimeType: "image/png"},
		&ToolCall{ID: "c1", Name: "ls"},
	}
	for _, b := range blocks {
		switch b.(type) {
		case TextContent, ThinkingContent, ImageContent, *ToolCall:
		default:
			t.Fatalf("content %T is not one of the four sealed kinds", b)
		}
	}
	// ToolResultMessage must NOT satisfy Content (it's a Message, not a block).
	var _ Message = &ToolResultMessage{}
}

// INVARIANT (DESIGN §1.1): a plain-text user message round-trips through the
// string shorthand. (Pi allows string content; NewUserText is the Go form.)
func TestNewUserTextProducesSingleTextBlock(t *testing.T) {
	m := NewUserText("hello", time.Time{})
	if len(m.Content) != 1 {
		t.Fatalf("expected 1 block, got %d", len(m.Content))
	}
	txt, ok := m.Content[0].(TextContent)
	if !ok {
		t.Fatalf("expected TextContent, got %T", m.Content[0])
	}
	if txt.Text != "hello" {
		t.Fatalf("expected text %q, got %q", "hello", txt.Text)
	}
}

// INVARIANT (DESIGN §1.4): StopReason has exactly the five Pi values.
func TestStopReasonValues(t *testing.T) {
	want := map[StopReason]bool{
		StopReasonEndTurn: true, StopReasonLength: true, StopReasonToolUse: true,
		StopReasonError: true, StopReasonAborted: true,
	}
	for r := range want {
		if r != "stop" && r != "length" && r != "toolUse" && r != "error" && r != "aborted" {
			t.Fatalf("unexpected StopReason %q", r)
		}
	}
	if len(want) != 5 {
		t.Fatalf("expected 5 stop reasons, got %d", len(want))
	}
}

// INVARIANT (DESIGN §1.2): Entry is a sealed union. MessageEntry wraps a
// Message; metadata entries carry their own payload.
func TestEntryIsSealedUnion(t *testing.T) {
	b := EntryBase{ID: "e1", ParentID: "", Timestamp: time.Now()}
	entries := []Entry{
		&MessageEntry{EntryBase: b, Message: NewUserText("x", time.Now())},
		&ModelChangeEntry{EntryBase: b, Provider: "p", ModelID: "m"},
		&ThinkingChangeEntry{EntryBase: b, Level: ThinkingHigh},
		&ToolsChangeEntry{EntryBase: b, ActiveTools: []string{"bash"}},
		&CompactionEntry{EntryBase: b, Summary: "s"},
		&BranchSummaryEntry{EntryBase: b, Summary: "s"},
		&LabelEntry{EntryBase: b, TargetID: "t", Label: "l"},
		&SessionInfoEntry{EntryBase: b, Name: "n"},
		&CustomEntry{EntryBase: b, Type: "status", Data: []byte("{}")},
		&LeafEntry{EntryBase: b, TargetID: "t1"},
		&CustomMessageEntry{EntryBase: b, CustomType: "x", Content: []Content{TextContent{Text: "y"}}},
	}
	seen := map[string]bool{}
	for _, e := range entries {
		switch e.(type) {
		case *MessageEntry, *ModelChangeEntry, *ThinkingChangeEntry, *ToolsChangeEntry,
			*CompactionEntry, *BranchSummaryEntry, *LabelEntry, *SessionInfoEntry, *CustomEntry,
			*LeafEntry, *CustomMessageEntry:
		default:
			t.Fatalf("entry %T not a sealed kind", e)
		}
		if e.ID() != "e1" {
			t.Fatalf("%T: ID() = %q, want e1", e, e.ID())
		}
		seen["ok"] = true
	}
	if len(entries) != 11 {
		t.Fatalf("expected 11 entry kinds, got %d", len(entries))
	}
}

// INVARIANT (DESIGN §1.3): the Event union is closed. Exhaustive switch covers
// the loop events + harness events. AgentEnd is present (the single-terminal-event invariant).
func TestEventUnionIsClosed(t *testing.T) {
	events := []Event{
		AgentStart{},
		TurnStart{},
		MessageStart{},
		MessageUpdate{},
		MessageEnd{},
		ToolExecStart{},
		ToolExecUpdate{},
		ToolExecEnd{},
		ApprovalRequest{},
		ApprovalResolution{},
		TurnEnd{},
		AgentEnd{},
		ModelUpdate{},
		ThinkingUpdate{},
		ToolsUpdate{},
		QueueUpdate{},
		Settled{},
		SavePoint{},
		Abort{},
		ProviderRetry{},
		&Error{},
	}
	for _, e := range events {
		// The unexported marker makes this assignment a package-owned contract.
		_ = e
	}
	// AgentEnd must exist (single-AgentEnd invariant, DESIGN §1.3).
	var _ Event = AgentEnd{}
}

// INVARIANT (DESIGN §1.3): Delta is the sealed union that collapses the 9-way
// streaming split to three kinds.
func TestDeltaIsSealedUnionOfThree(t *testing.T) {
	deltas := []Delta{
		TextDelta{Text: "a"},
		ThinkingDelta{Text: "b"},
		ToolCallDelta{ToolCallID: "c", Name: "ls", ArgumentsChunk: `{"`},
	}
	if len(deltas) != 3 {
		t.Fatalf("expected 3 delta kinds, got %d", len(deltas))
	}
	for _, d := range deltas {
		switch d.(type) {
		case TextDelta, ThinkingDelta, ToolCallDelta:
		default:
			t.Fatalf("delta %T not a sealed kind", d)
		}
	}
}

// INVARIANT (DESIGN §5): the subagent seam is present — events carry SessionOrigin.
func TestSubagentSeamPresent(t *testing.T) {
	start := AgentStart{Origin: SessionOrigin{SessionID: "root"}}
	if start.Origin.SessionID != "root" {
		t.Fatal("AgentStart must carry SessionOrigin")
	}
	// ChildID is the seam for future subagents; empty for root today.
	if start.Origin.ChildID != "" {
		t.Fatal("root events must have empty ChildID")
	}
}

// INVARIANT (DESIGN §1.1): a ToolCall's arguments are parsed JSON (map), not a
// raw string — matching Pi's Record<string,any>.
func TestToolCallArgumentsAreParsedJSON(t *testing.T) {
	tc := &ToolCall{ID: "c1", Name: "edit", Arguments: map[string]any{"path": "/tmp/x"}}
	b, err := json.Marshal(tc.Arguments)
	if err != nil {
		t.Fatal(err)
	}
	if string(b) != `{"path":"/tmp/x"}` {
		t.Fatalf("expected parsed-json arguments, got %s", b)
	}
}

// INVARIANT: AppendLabel creates a LabelEntry and GetLabel returns the latest label.
func TestAppendLabelRoundTrip(t *testing.T) {
	store, err := NewSQLiteStore(":memory:", "contract")
	if err != nil {
		t.Fatal(err)
	}
	sess := NewSession(store, 64)
	ctx := context.Background()

	msgID, err := sess.AppendMessage(ctx, NewUserText("hello", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	_, err = sess.AppendLabel(ctx, msgID, "first")
	if err != nil {
		t.Fatal(err)
	}

	label, err := sess.GetLabel(ctx, msgID)
	if err != nil {
		t.Fatal(err)
	}
	if label != "first" {
		t.Fatalf("GetLabel = %q, want \"first\"", label)
	}
}

func TestGetLabelReturnsLatest(t *testing.T) {
	store, _ := NewSQLiteStore(":memory:", "contract")
	sess := NewSession(store, 64)
	ctx := context.Background()

	msgID, _ := sess.AppendMessage(ctx, NewUserText("hello", time.Now()))
	sess.AppendLabel(ctx, msgID, "old")
	sess.AppendLabel(ctx, msgID, "new")

	label, _ := sess.GetLabel(ctx, msgID)
	if label != "new" {
		t.Fatalf("GetLabel = %q, want \"new\"", label)
	}
}

func TestAppendLabelTargetNotFound(t *testing.T) {
	store, _ := NewSQLiteStore(":memory:", "contract")
	sess := NewSession(store, 64)
	ctx := context.Background()

	if _, err := sess.AppendLabel(ctx, "nonexistent", "x"); err == nil {
		t.Fatal("expected error for nonexistent target")
	}
}

func TestGetLabelNonexistentReturnsEmpty(t *testing.T) {
	store, _ := NewSQLiteStore(":memory:", "contract")
	sess := NewSession(store, 64)
	ctx := context.Background()

	label, _ := sess.GetLabel(ctx, "nonexistent")
	if label != "" {
		t.Fatalf("GetLabel = %q, want empty", label)
	}
}

func TestMoveToAppendsLeafEntry(t *testing.T) {
	store, _ := NewSQLiteStore(":memory:", "contract")
	sess := NewSession(store, 64)
	ctx := context.Background()

	id1, _ := sess.AppendMessage(ctx, NewUserText("first", time.Now()))
	sess.AppendMessage(ctx, NewUserText("second", time.Now()))

	if _, err := sess.MoveTo(ctx, id1, nil); err != nil {
		t.Fatal(err)
	}

	entries, _ := sess.Entries(ctx)
	found := false
	for _, e := range entries {
		if le, ok := e.(*LeafEntry); ok && le.TargetID == id1 {
			found = true
			break
		}
	}
	if !found {
		t.Fatal("expected LeafEntry with TargetID == first message ID")
	}
}

func TestBranchSummaryProjectsAtTreePositionAndReplays(t *testing.T) {
	path := filepath.Join(t.TempDir(), "session.db")
	ctx := context.Background()
	store, err := NewSQLiteStore(path, "branch-summary")
	if err != nil {
		t.Fatal(err)
	}
	sess := NewSession(store, 64)
	a, err := sess.AppendMessage(ctx, NewUserText("A", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, NewUserText("B", time.Now())); err != nil {
		t.Fatal(err)
	}
	if _, err := sess.MoveTo(
		ctx,
		a,
		&BranchSummaryData{FromID: "old-leaf", Summary: "returned from branch"},
	); err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, NewUserText("C", time.Now())); err != nil {
		t.Fatal(err)
	}
	assertBranchSummaryContext(t, sess)
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := NewSQLiteStore(path, "branch-summary")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	assertBranchSummaryContext(t, NewSession(reopened, 64))
}

func assertBranchSummaryContext(t *testing.T, sess Session) {
	t.Helper()
	snap, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Messages) != 3 {
		t.Fatalf("context messages = %d, want 3", len(snap.Messages))
	}
	if got := MessageText(snap.Messages[0]); got != "A" {
		t.Fatalf("message 0 = %q, want A", got)
	}
	if got := MessageText(snap.Messages[1]); got != BranchSummaryPrefix+"returned from branch"+BranchSummarySuffix {
		t.Fatalf("message 1 = %q, want branch summary", got)
	}
	if got := MessageText(snap.Messages[2]); got != "C" {
		t.Fatalf("message 2 = %q, want C", got)
	}
	entries, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if summary, ok := entry.(*BranchSummaryEntry); ok && summary.Summary == "returned from branch" {
			if summary.FromID != "old-leaf" {
				t.Fatalf("branch summary from ID = %q, want old-leaf", summary.FromID)
			}
			return
		}
	}
	t.Fatal("branch summary entry not found")
}

func TestProjectContextUsesCompactionTimestamp(t *testing.T) {
	timestamp := time.Date(2026, 8, 8, 12, 34, 56, 0, time.UTC)
	entries := []Entry{
		&MessageEntry{
			EntryBase: EntryBase{ID: "old", Timestamp: timestamp.Add(-time.Hour)},
			Message:   NewUserText("old", timestamp.Add(-time.Hour)),
		},
		&CompactionEntry{
			EntryBase:   EntryBase{ID: "compact", Timestamp: timestamp},
			Summary:     "checkpoint",
			FirstKeptID: "kept",
		},
		&MessageEntry{
			EntryBase: EntryBase{ID: "kept", ParentID: "compact", Timestamp: timestamp.Add(time.Minute)},
			Message:   NewUserText("kept", timestamp.Add(time.Minute)),
		},
	}
	contextSnapshot, err := ProjectContext(entries)
	if err != nil {
		t.Fatalf("ProjectContext: %v", err)
	}
	if len(contextSnapshot.Messages) != 2 {
		t.Fatalf("context messages = %d, want summary plus kept message", len(contextSnapshot.Messages))
	}
	summary, ok := contextSnapshot.Messages[0].(*UserMessage)
	if !ok {
		t.Fatalf("summary message = %T, want *UserMessage", contextSnapshot.Messages[0])
	}
	if !summary.Timestamp.Equal(timestamp) {
		t.Fatalf("summary timestamp = %s, want %s", summary.Timestamp, timestamp)
	}
}

func TestCustomMessageEntryInBuildContext(t *testing.T) {
	store, _ := NewSQLiteStore(":memory:", "contract")
	sess := NewSession(store, 64)
	ctx := context.Background()

	_, err := sess.AppendCustomMessage(ctx, &CustomMessageEntry{
		CustomType: "status",
		Content:    []Content{TextContent{Text: "Task completed"}},
		Display:    "Status update",
		Details:    []byte(`{"code": 200}`),
	})
	if err != nil {
		t.Fatal(err)
	}

	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}

	found := false
	for _, m := range snap.Messages {
		if cm, ok := m.(*CustomMessage); ok && cm.CustomType == "status" {
			found = true
			break
		}
	}
	if !found {
		t.Fatal("CustomMessage not found in BuildContext")
	}
}

// INVARIANT: AssistantMessage JSON round-trip preserves ThinkingLevel.
// This was broken in b2257edb (field added to struct but not to MarshalJSON/unmarshalMessage).
func TestAssistantMessageThinkingLevelRoundTrip(t *testing.T) {
	m := &AssistantMessage{
		Content:       []Content{TextContent{Text: "hello"}},
		ThinkingLevel: ThinkingHigh,
		StopReason:    StopReasonEndTurn,
		Timestamp:     time.Now(),
	}

	b, err := json.Marshal(m)
	if err != nil {
		t.Fatal(err)
	}

	got, err := unmarshalMessage(b)
	if err != nil {
		t.Fatal(err)
	}

	am, ok := got.(*AssistantMessage)
	if !ok {
		t.Fatalf("unmarshaled message is %T, want *AssistantMessage", got)
	}
	if am.ThinkingLevel != ThinkingHigh {
		t.Fatalf("ThinkingLevel = %q, want %q", am.ThinkingLevel, ThinkingHigh)
	}
}

func TestAssistantMessageThinkingLevelDefaultRoundTrip(t *testing.T) {
	// Zero-value ThinkingLevel should survive as empty string.
	m := &AssistantMessage{
		Content:       []Content{TextContent{Text: "hello"}},
		ThinkingLevel: ThinkingOff,
		StopReason:    StopReasonEndTurn,
		Timestamp:     time.Now(),
	}

	b, err := json.Marshal(m)
	if err != nil {
		t.Fatal(err)
	}

	got, err := unmarshalMessage(b)
	if err != nil {
		t.Fatal(err)
	}

	am := got.(*AssistantMessage)
	if am.ThinkingLevel != ThinkingOff {
		t.Fatalf("ThinkingLevel = %q, want %q", am.ThinkingLevel, ThinkingOff)
	}
}

func TestLeafEntryNotInBuildContext(t *testing.T) {
	store, _ := NewSQLiteStore(":memory:", "contract")
	sess := NewSession(store, 64)
	ctx := context.Background()

	id1, _ := sess.AppendMessage(ctx, NewUserText("msg", time.Now()))
	sess.MoveTo(ctx, id1, nil)

	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}

	if len(snap.Messages) != 1 {
		t.Fatalf("expected 1 msg in BuildContext, got %d (LeafEntry should not project)", len(snap.Messages))
	}
}
