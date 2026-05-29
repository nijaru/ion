package session

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestContextAddedIsModelVisibleButNotTranscript(t *testing.T) {
	sess := New("context-session")
	if err := sess.AppendContext(t.Context(), ContextEntry{
		Kind:    ContextKindBootstrap,
		Content: "workspace context",
	}); err != nil {
		t.Fatalf("AppendContext: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "hello"); err != nil {
		t.Fatalf("AppendUser: %v", err)
	}

	if got := sess.Messages(); len(got) != 1 || got[0].Content != "hello" {
		t.Fatalf("expected raw transcript only, got %#v", got)
	}

	effective, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(effective) != 2 {
		t.Fatalf("expected context plus transcript, got %#v", effective)
	}
	if effective[0].Role != llm.RoleUser ||
		!strings.Contains(effective[0].Content, "workspace context") {
		t.Fatalf("expected context as user-role history, got %#v", effective[0])
	}
	if effective[1].Content != "hello" {
		t.Fatalf("expected transcript after context, got %#v", effective)
	}
}

func TestEffectiveEntriesPreserveContextMarkers(t *testing.T) {
	sess := New("context-entry-session")
	if err := sess.AppendContext(t.Context(), ContextEntry{
		Kind:    ContextKindHarness,
		Content: "harness context",
	}); err != nil {
		t.Fatalf("AppendContext: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "hello"); err != nil {
		t.Fatalf("AppendUser: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("expected context plus transcript, got %#v", entries)
	}
	if entries[0].EventType != ContextAdded ||
		entries[0].ContextKind != ContextKindHarness ||
		entries[0].ContextPlacement != ContextPlacementPrefix {
		t.Fatalf("expected context markers on first entry, got %#v", entries[0])
	}
	if entries[1].EventType != MessageAdded || entries[1].ContextKind != "" {
		t.Fatalf("expected transcript markers on second entry, got %#v", entries[1])
	}
}

func TestEffectiveEntriesPlacePrefixContextBeforeTranscript(t *testing.T) {
	sess := New("context-entry-order-session")
	if err := sess.AppendUser(t.Context(), "first user"); err != nil {
		t.Fatalf("AppendUser first: %v", err)
	}
	if err := sess.AppendContext(t.Context(), ContextEntry{
		Kind:      ContextKindGeneric,
		Placement: ContextPlacementPrefix,
		Content:   "stable context",
	}); err != nil {
		t.Fatalf("AppendContext: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "second user"); err != nil {
		t.Fatalf("AppendUser second: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("expected context plus transcript, got %#v", entries)
	}
	if entries[0].ContextPlacement != ContextPlacementPrefix ||
		entries[0].Message.Content != "stable context" {
		t.Fatalf("expected prefix context first, got %#v", entries)
	}
	if entries[1].Message.Content != "first user" ||
		entries[2].Message.Content != "second user" {
		t.Fatalf("expected transcript after prefix context, got %#v", entries)
	}
}

func TestRebuilderKeepsWorkingSetAfterDurableContextEntries(t *testing.T) {
	sess := New("context-rebuild-session")
	if err := sess.AppendContext(t.Context(), ContextEntry{
		Kind:    ContextKindBootstrap,
		Content: "bootstrap context without XML tags",
	}); err != nil {
		t.Fatalf("AppendContext: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "hello"); err != nil {
		t.Fatalf("AppendUser: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries before compaction: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("expected two effective entries, got %#v", entries)
	}
	snapshot := CompactionSnapshot{
		Strategy:      "offload",
		CutoffEventID: entries[1].EventID,
		Entries:       entries,
		ReadFiles:     []string{"agent/handoff.go"},
	}
	if err := sess.Append(t.Context(), NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction snapshot: %v", err)
	}

	rebuilt, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries after compaction: %v", err)
	}
	if len(rebuilt) != 3 {
		t.Fatalf("expected bootstrap context, working set, transcript, got %#v", rebuilt)
	}
	if rebuilt[0].ContextKind != ContextKindBootstrap {
		t.Fatalf("expected bootstrap context first, got %#v", rebuilt[0])
	}
	if rebuilt[1].ContextKind != ContextKindWorkingSet {
		t.Fatalf("expected working set after durable context, got %#v", rebuilt[1])
	}
	if rebuilt[0].ContextPlacement != ContextPlacementPrefix ||
		rebuilt[1].ContextPlacement != ContextPlacementPrefix {
		t.Fatalf("expected stable context prefix placements, got %#v", rebuilt[:2])
	}
	if !strings.Contains(rebuilt[1].Message.Content, "agent/handoff.go") {
		t.Fatalf("expected working set file reference, got %#v", rebuilt[1].Message)
	}
	if rebuilt[2].EventType != MessageAdded || rebuilt[2].Message.Content != "hello" {
		t.Fatalf("expected transcript after context entries, got %#v", rebuilt[2])
	}
}
