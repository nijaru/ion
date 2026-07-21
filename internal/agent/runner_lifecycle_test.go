package agent

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestRunnerPersistEntryAdvancesLeaf(t *testing.T) {
	store := newTestStore(t)
	h := NewController(ControllerConfig{Session: session.NewSession(store, 64), Store: store})
	if _, err := h.Session().AppendMessage(context.Background(), session.NewUserText("prior", time.Now())); err != nil {
		t.Fatalf("append prior message: %v", err)
	}
	entry := &session.CustomEntry{
		EntryBase: session.EntryBase{ID: "runner-entry", Timestamp: time.Now()},
		Type:      "runner_test",
		Data:      []byte(`{"ok":true}`),
	}
	if err := h.PersistEntry(context.Background(), entry); err != nil {
		t.Fatalf("persist: %v", err)
	}
	if got := store.GetLeafID(); got != entry.ID() {
		t.Fatalf("leaf = %q, want persisted entry %q", got, entry.ID())
	}
	branch, err := store.Branch(context.Background())
	if err != nil {
		t.Fatalf("branch: %v", err)
	}
	if len(branch) != 2 || session.EntryText(branch[0]) != "prior" || branch[1].ID() != entry.ID() {
		t.Fatalf("branch = %#v, want prior message followed by runner entry", branch)
	}
	if err := h.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
}

func TestRunnerPendingEntriesRemainOrdered(t *testing.T) {
	store := newTestStore(t)
	h := NewController(ControllerConfig{Session: session.NewSession(store, 64), Store: store})
	h.phase = PhaseStreaming
	first := &session.CustomEntry{EntryBase: session.EntryBase{ID: "pending-one", Timestamp: time.Now()}, Type: "pending"}
	second := &session.CustomEntry{EntryBase: session.EntryBase{ID: "pending-two", Timestamp: time.Now()}, Type: "pending"}
	if err := h.PersistEntry(context.Background(), first); err != nil {
		t.Fatalf("queue first: %v", err)
	}
	if err := h.PersistEntry(context.Background(), second); err != nil {
		t.Fatalf("queue second: %v", err)
	}
	h.phase = PhaseReady
	h.flushPending(context.Background())
	branch, err := store.Branch(context.Background())
	if err != nil {
		t.Fatalf("branch: %v", err)
	}
	if len(branch) != 2 || branch[0].ID() != first.ID() || branch[1].ID() != second.ID() {
		t.Fatalf("branch IDs = %v, want [%q %q]", entryIDs(branch), first.ID(), second.ID())
	}
	_ = h.Close()
}

func entryIDs(entries []session.Entry) []string {
	ids := make([]string, len(entries))
	for i, entry := range entries {
		ids[i] = entry.ID()
	}
	return ids
}

func TestRunnerMutationsRejectAfterClose(t *testing.T) {
	store := newTestStore(t)
	h := NewController(ControllerConfig{Session: session.NewSession(store, 64), Store: store})
	if err := h.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	ctx := context.Background()
	if _, err := h.Prompt(ctx, "late"); err == nil {
		t.Fatal("prompt after close succeeded")
	}
	if err := h.PersistEntry(ctx, &session.CustomEntry{}); err == nil {
		t.Fatal("persist after close succeeded")
	}
	if _, err := h.AppendSessionInfo(ctx, "late"); err == nil {
		t.Fatal("session info append after close succeeded")
	}
	if _, err := h.NavigateTree(ctx, "missing", NavigateOptions{}); err == nil {
		t.Fatal("tree navigation after close succeeded")
	}
	if _, err := h.AppendLabel(ctx, "missing", "late"); err == nil {
		t.Fatal("label append after close succeeded")
	}
	if _, err := h.GetLabel(ctx, "missing"); err == nil {
		t.Fatal("label read after close succeeded")
	}
	h.NextTurn("late")
	if len(h.nextTurn) != 0 {
		t.Fatal("next-turn queue changed after close")
	}
}
