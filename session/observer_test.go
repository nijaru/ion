package session

import (
	"context"
	"testing"
	"time"
)

func TestObserveEventsReceivesAppendsInOrder(t *testing.T) {
	sess := New("observed-order")
	events := []Event{
		NewEvent("observed-order", Handoff, map[string]int{"index": 0}),
		NewEvent("observed-order", Handoff, map[string]int{"index": 1}),
		NewEvent("observed-order", Handoff, map[string]int{"index": 2}),
	}

	var got []string
	detach := sess.ObserveEvents(func(_ context.Context, event Event) error {
		got = append(got, event.ID.String())
		return nil
	})
	defer detach()

	for _, event := range events {
		if err := sess.Append(t.Context(), event); err != nil {
			t.Fatalf("Append: %v", err)
		}
	}

	if len(got) != len(events) {
		t.Fatalf("observed events = %d, want %d", len(got), len(events))
	}
	for i, event := range events {
		if got[i] != event.ID.String() {
			t.Fatalf("observed event %d = %q, want %q", i, got[i], event.ID)
		}
	}
}

func TestObserveEventsBackpressuresAppend(t *testing.T) {
	sess := New("observed-backpressure")
	entered := make(chan struct{})
	release := make(chan struct{})

	detach := sess.ObserveEvents(func(ctx context.Context, _ Event) error {
		close(entered)
		select {
		case <-release:
			return nil
		case <-ctx.Done():
			return ctx.Err()
		}
	})
	defer detach()

	done := make(chan error, 1)
	go func() {
		done <- sess.Append(t.Context(), NewEvent("observed-backpressure", Handoff, nil))
	}()

	select {
	case <-entered:
	case <-time.After(time.Second):
		t.Fatal("observer was not called")
	}

	select {
	case err := <-done:
		t.Fatalf("Append returned before observer released: %v", err)
	case <-time.After(25 * time.Millisecond):
	}

	close(release)
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Append after release: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Append did not return after observer release")
	}
}

func TestObserveEventsDetachStopsDelivery(t *testing.T) {
	sess := New("observed-detach")
	observed := make(chan Event, 1)
	detach := sess.ObserveEvents(func(_ context.Context, event Event) error {
		observed <- event
		return nil
	})
	detach()

	if err := sess.Append(t.Context(), NewEvent("observed-detach", Handoff, nil)); err != nil {
		t.Fatalf("Append: %v", err)
	}

	select {
	case event := <-observed:
		t.Fatalf("observed event after detach: %s", event.ID)
	case <-time.After(25 * time.Millisecond):
	}
}
