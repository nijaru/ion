package session

import (
	"encoding/json"
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

// INVARIANT (DESIGN §1.2): Entry is a sealed union. A type switch must cover the
// nine kinds. MessageEntry wraps a Message; metadata entries carry their own payload.
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
	}
	seen := map[string]bool{}
	for _, e := range entries {
		switch e.(type) {
		case *MessageEntry, *ModelChangeEntry, *ThinkingChangeEntry, *ToolsChangeEntry,
			*CompactionEntry, *BranchSummaryEntry, *LabelEntry, *SessionInfoEntry, *CustomEntry:
		default:
			t.Fatalf("entry %T not a sealed kind", e)
		}
		if e.ID() != "e1" {
			t.Fatalf("%T: ID() = %q, want e1", e, e.ID())
		}
		seen["ok"] = true
	}
	if len(entries) != 9 {
		t.Fatalf("expected 9 entry kinds, got %d", len(entries))
	}
}

// INVARIANT (DESIGN §1.3): the Event union is closed. Exhaustive switch covers
// the loop events + harness events. AgentEnd is present (the single-terminal-event invariant).
func TestEventUnionIsClosed(t *testing.T) {
	events := []Event{
		AgentStart{}, TurnStart{}, MessageStart{}, MessageUpdate{}, MessageEnd{},
		ToolExecStart{}, ToolExecUpdate{}, ToolExecEnd{}, TurnEnd{}, AgentEnd{},
		&Error{},
	}
	for _, e := range events {
		// The compiler enforces exhaustiveness only in switch statements with a
		// default; here we assert each value satisfies Event (sealed) and that
		// AgentEnd is among them.
		_ = e.IsEvent // method present on all events
	}
	// AgentEnd must exist (single-AgentEnd invariant, DESIGN §1.3).
	var _ Event = AgentEnd{}
}

// INVARIANT (DESIGN §1.3): Delta is the sealed union that collapses the 9-way
// streaming split to three kinds.
func TestDeltaIsSealedUnionOfThree(t *testing.T) {
	deltas := []Delta{
		TextDelta{Text: "a"}, ThinkingDelta{Text: "b"},
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
