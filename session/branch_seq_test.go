package session

import (
	"context"
	"testing"
	"time"
)

func TestBranchSeqIteration(t *testing.T) {
	tempDir := t.TempDir()
	store, err := NewSQLiteStore(tempDir, "seq-test-session")
	if err != nil {
		t.Fatalf("failed to create sqlite store: %v", err)
	}
	defer store.Close()

	ctx := context.Background()
	now := time.Now()

	e1 := &MessageEntry{
		EntryBase: EntryBase{ID: "e-1", Timestamp: now},
		Message:   NewUserText("First user message", now),
	}
	if _, err := store.AppendLeafEntry(ctx, e1); err != nil {
		t.Fatalf("failed to append e1: %v", err)
	}

	e2 := &MessageEntry{
		EntryBase: EntryBase{ID: "e-2", ParentID: "e-1", Timestamp: now.Add(time.Second)},
		Message: &AssistantMessage{
			Content:   []Content{TextContent{Text: "Assistant response"}},
			Timestamp: now.Add(time.Second),
		},
	}
	if _, err := store.AppendLeafEntry(ctx, e2); err != nil {
		t.Fatalf("failed to append e2: %v", err)
	}

	// Test BranchSeq iteration
	var collected []string
	for entry, err := range store.BranchSeq(ctx) {
		if err != nil {
			t.Fatalf("BranchSeq yielded error: %v", err)
		}
		collected = append(collected, entry.ID())
	}

	if len(collected) != 2 || collected[0] != "e-1" || collected[1] != "e-2" {
		t.Fatalf("BranchSeq collected = %#v, want [e-1, e-2]", collected)
	}

	// Test early break from BranchSeq iterator
	var count int
	for entry, err := range store.BranchSeq(ctx) {
		if err != nil {
			t.Fatalf("BranchSeq error: %v", err)
		}
		if entry.ID() == "e-1" {
			count++
			break
		}
	}
	if count != 1 {
		t.Fatalf("expected early break to stop after 1 item, got %d", count)
	}

	// Test session façade BranchSeq
	sess := NewSession(store, 16)
	var sessCollected []string
	for entry, err := range sess.BranchSeq(ctx) {
		if err != nil {
			t.Fatalf("sess.BranchSeq error: %v", err)
		}
		sessCollected = append(sessCollected, entry.ID())
	}
	if len(sessCollected) != 2 {
		t.Fatalf("expected 2 entries in sess.BranchSeq, got %d", len(sessCollected))
	}

	// Test EntriesSeq
	var allEntries []string
	for entry, err := range store.EntriesSeq(ctx) {
		if err != nil {
			t.Fatalf("store.EntriesSeq error: %v", err)
		}
		allEntries = append(allEntries, entry.ID())
	}
	if len(allEntries) != 2 {
		t.Fatalf("expected 2 entries in store.EntriesSeq, got %d", len(allEntries))
	}

	// Test empty leaf BranchAtSeq
	for _, err := range store.BranchAtSeq(ctx, "") {
		if err != nil {
			t.Fatalf("unexpected error on empty leaf: %v", err)
		}
		t.Fatal("expected 0 iterations on empty leaf")
	}
}
