package agent

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestEventStreamBroadcastsIndependentOrderedSubscriptions(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: newTestSession(t)})
	defer h.Close()

	first, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	second, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	defer second.Close()

	for _, event := range []session.Event{session.AgentStart{}, session.TurnStart{}, session.Settled{}} {
		h.emit(event)
	}
	for i := range 3 {
		left := receiveEvent(t, first)
		right := receiveEvent(t, second)
		if left.Stream != right.Stream || left.Sequence != right.Sequence {
			t.Fatalf("subscription %d identity mismatch: %#v vs %#v", i, left, right)
		}
		if got, want := left.Sequence, uint64(i+1); got != want {
			t.Fatalf("event %d sequence = %d, want %d", i, got, want)
		}
	}
}

func TestEventStreamDetachesSlowSubscriberAtBound(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: newTestSession(t)})
	defer h.Close()

	sub, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < eventSubscriberCapacity+1; i++ {
		h.emit(session.ProviderRetry{Attempt: i + 1})
	}
	if !errors.Is(sub.Err(), ErrSubscriptionLagged) {
		t.Fatalf("subscription error = %v, want ErrSubscriptionLagged", sub.Err())
	}
	count := 0
	for range sub.Events {
		count++
	}
	if count != eventSubscriberCapacity {
		t.Fatalf("delivered events = %d, want bounded prefix %d", count, eventSubscriberCapacity)
	}
}

func TestEventStreamResubscriptionReturnsFreshSnapshot(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: newTestSession(t)})
	defer h.Close()

	first, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	initial := first.Snapshot.Cursor
	first.Close()
	h.emit(session.AgentStart{})

	second, err := h.Subscribe(context.Background(), initial)
	if err != nil {
		t.Fatal(err)
	}
	defer second.Close()
	if !second.Snapshot.Resynced {
		t.Fatal("resubscription did not mark snapshot as resynced")
	}
	if got := second.Snapshot.Cursor.Next; got != initial.Next+1 {
		t.Fatalf("snapshot next cursor = %d, want %d", got, initial.Next+1)
	}
}

func TestEventStreamCloseDoesNotBlockWithoutSubscribers(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: newTestSession(t)})
	done := make(chan error, 1)
	go func() { done <- h.Close() }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close blocked without subscribers")
	}
}

func receiveEvent(t *testing.T, sub *EventSubscription) EventEnvelope {
	t.Helper()
	select {
	case event, ok := <-sub.Events:
		if !ok {
			t.Fatalf("subscription closed: %v", sub.Err())
		}
		return event
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for event")
		return EventEnvelope{}
	}
}
