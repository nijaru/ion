package session

import (
	"errors"
	"path/filepath"
	"testing"
)

func TestSessionAppendAssignsSequenceAndTurnID(t *testing.T) {
	sess := New("identity-session")
	ctx := WithTurnID(t.Context(), "turn-1")

	if err := sess.Append(ctx, NewEvent(sess.ID(), Handoff, map[string]string{"n": "one"})); err != nil {
		t.Fatalf("Append first: %v", err)
	}
	if err := sess.Append(ctx, NewEvent(sess.ID(), ExternalInput, map[string]string{"n": "two"})); err != nil {
		t.Fatalf("Append second: %v", err)
	}

	events := sess.Events()
	if len(events) != 2 {
		t.Fatalf("events = %d, want 2", len(events))
	}
	for i, event := range events {
		wantSeq := int64(i + 1)
		if event.Seq != wantSeq {
			t.Fatalf("event %d seq = %d, want %d", i, event.Seq, wantSeq)
		}
		if event.TurnID != "turn-1" {
			t.Fatalf("event %d turn id = %q, want turn-1", i, event.TurnID)
		}
	}
}

func TestSessionAppendRejectsNonMonotonicExplicitSequence(t *testing.T) {
	sess := New("sequence-session")
	first := NewEvent("sequence-session", Handoff, nil)
	first.Seq = 2
	if err := sess.Append(t.Context(), first); err != nil {
		t.Fatalf("Append first explicit seq: %v", err)
	}

	second := NewEvent("sequence-session", ExternalInput, nil)
	second.Seq = 1
	err := sess.Append(t.Context(), second)
	if !errors.Is(err, errNonMonotonicEventSequence) {
		t.Fatalf("Append non-monotonic error = %v, want %v", err, errNonMonotonicEventSequence)
	}
}

func TestReplayerAdvancesSequenceForLegacyEvents(t *testing.T) {
	replayer := NewReplayer()
	sess := replayer.NewSession("legacy-sequence")
	if err := replayer.Apply(sess, NewEvent(sess.ID(), Handoff, nil)); err != nil {
		t.Fatalf("Apply first legacy event: %v", err)
	}
	if err := replayer.Apply(sess, NewEvent(sess.ID(), ExternalInput, nil)); err != nil {
		t.Fatalf("Apply second legacy event: %v", err)
	}
	if err := sess.Append(t.Context(), NewEvent(sess.ID(), WaitStarted, WaitData{Reason: "next"})); err != nil {
		t.Fatalf("Append after replay: %v", err)
	}

	events := sess.Events()
	if got := events[2].Seq; got != 3 {
		t.Fatalf("new event seq = %d, want 3", got)
	}
}

func TestStoresPersistSequenceAndTurnID(t *testing.T) {
	t.Run("jsonl", func(t *testing.T) {
		store, err := NewJSONLStore(t.TempDir())
		if err != nil {
			t.Fatalf("NewJSONLStore: %v", err)
		}
		defer store.Close()
		assertStorePersistsSequenceAndTurnID(t, store, "jsonl-identity")
	})

	t.Run("sqlite", func(t *testing.T) {
		store, err := NewSQLiteStore(filepath.Join(t.TempDir(), "sessions.db"))
		if err != nil {
			t.Fatalf("NewSQLiteStore: %v", err)
		}
		defer store.Close()
		assertStorePersistsSequenceAndTurnID(t, store, "sqlite-identity")
	})
}

func TestStoresReturnEventsAfterSequence(t *testing.T) {
	t.Run("jsonl", func(t *testing.T) {
		store, err := NewJSONLStore(t.TempDir())
		if err != nil {
			t.Fatalf("NewJSONLStore: %v", err)
		}
		defer store.Close()
		assertStoreReturnsEventsAfterSequence(t, store, "jsonl-events-after")
	})

	t.Run("sqlite", func(t *testing.T) {
		store, err := NewSQLiteStore(filepath.Join(t.TempDir(), "sessions.db"))
		if err != nil {
			t.Fatalf("NewSQLiteStore: %v", err)
		}
		defer store.Close()
		assertStoreReturnsEventsAfterSequence(t, store, "sqlite-events-after")
	})
}

func assertStorePersistsSequenceAndTurnID(t *testing.T, store Store, sessionID string) {
	t.Helper()

	sess := New(sessionID).WithWriter(store)
	ctx := WithTurnID(t.Context(), "turn-store")
	if err := sess.Append(ctx, NewEvent(sessionID, Handoff, nil)); err != nil {
		t.Fatalf("Append first: %v", err)
	}
	if err := sess.Append(ctx, NewEvent(sessionID, ExternalInput, nil)); err != nil {
		t.Fatalf("Append second: %v", err)
	}

	loaded, err := store.Load(t.Context(), sessionID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	events := loaded.Events()
	if len(events) != 2 {
		t.Fatalf("loaded events = %d, want 2", len(events))
	}
	for i, event := range events {
		wantSeq := int64(i + 1)
		if event.Seq != wantSeq {
			t.Fatalf("loaded event %d seq = %d, want %d", i, event.Seq, wantSeq)
		}
		if event.TurnID != "turn-store" {
			t.Fatalf("loaded event %d turn id = %q, want turn-store", i, event.TurnID)
		}
	}
}

func assertStoreReturnsEventsAfterSequence(
	t *testing.T,
	store interface {
		Store
		EventQueryStore
	},
	sessionID string,
) {
	t.Helper()

	sess := New(sessionID).WithWriter(store)
	ctx := WithTurnID(t.Context(), "turn-after")
	for _, eventType := range []EventType{Handoff, ExternalInput, WaitStarted, WaitResolved} {
		if err := sess.Append(ctx, NewEvent(sessionID, eventType, map[string]string{
			"type": string(eventType),
		})); err != nil {
			t.Fatalf("Append %s: %v", eventType, err)
		}
	}

	events, err := store.EventsAfter(t.Context(), sessionID, 2)
	if err != nil {
		t.Fatalf("EventsAfter: %v", err)
	}
	if len(events) != 2 {
		t.Fatalf("EventsAfter count = %d, want 2", len(events))
	}
	for i, event := range events {
		wantSeq := int64(i + 3)
		if event.Seq != wantSeq {
			t.Fatalf("event %d seq = %d, want %d", i, event.Seq, wantSeq)
		}
		if event.TurnID != "turn-after" {
			t.Fatalf("event %d turn id = %q, want turn-after", i, event.TurnID)
		}
	}
}
