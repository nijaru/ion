package session

import (
	"context"
	"sync"
	"testing"
	"time"
)

func TestWatch_ReceivesEvents(t *testing.T) {
	s := New("sess-1")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sub := s.Watch(ctx)
	defer sub.Close()

	e := NewUserMessage("sess-1", "hello")
	_ = s.Append(context.Background(), e)

	select {
	case got := <-sub.Events():
		if got.ID != e.ID {
			t.Fatalf("got event ID %v, want %v", got.ID, e.ID)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for event")
	}
}

func TestWatch_MultipleSubscribers(t *testing.T) {
	s := New("sess-2")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sub1 := s.Watch(ctx)
	defer sub1.Close()
	sub2 := s.Watch(ctx)
	defer sub2.Close()

	e := NewEvent("sess-2", Handoff, nil)
	_ = s.Append(context.Background(), e)

	for _, ch := range []<-chan Event{sub1.Events(), sub2.Events()} {
		select {
		case got := <-ch:
			if got.ID != e.ID {
				t.Fatalf("got event ID %v, want %v", got.ID, e.ID)
			}
		case <-time.After(time.Second):
			t.Fatal("timed out waiting for event on subscriber")
		}
	}
}

func TestWatch_ContextCancelClosesChannel(t *testing.T) {
	s := New("sess-3")
	ctx, cancel := context.WithCancel(context.Background())

	sub := s.Watch(ctx)
	cancel()

	// Channel should close shortly after cancel.
	select {
	case _, ok := <-sub.Events():
		if ok {
			t.Fatal("expected closed channel, got value")
		}
	case <-time.After(time.Second):
		t.Fatal("channel not closed after context cancel")
	}

	// Subscriber removed from list.
	s.mu.RLock()
	n := len(s.subscribers)
	s.mu.RUnlock()
	if n != 0 {
		t.Fatalf("subscriber list len = %d, want 0", n)
	}
}

func TestWatch_SlowSubscriberDoesNotBlock(t *testing.T) {
	s := New("sess-4")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// Subscribe but never read.
	sub := s.Watch(ctx)
	defer sub.Close()

	done := make(chan struct{})
	go func() {
		// Fill beyond buffer — Append must not block.
		for i := range subscriberBufSize + 10 {
			_ = i
			_ = s.Append(context.Background(), NewEvent("sess-4", Handoff, nil))
		}
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("Append blocked on slow subscriber")
	}
}

func TestWatch_NoSubscribers(t *testing.T) {
	s := New("sess-5")
	// Append with no subscribers must not panic.
	_ = s.Append(context.Background(), NewEvent("sess-5", Handoff, nil))
}

// TestWatch_ConcurrentAppendCancel exercises the race between Append and
// context cancellation. Run with -race to verify no data race or panic.
func TestWatch_ConcurrentAppendCancel(t *testing.T) {
	const goroutines = 8
	const eventsPerWriter = 200

	s := New("sess-race")
	ctx, cancel := context.WithCancel(context.Background())

	sub := s.Watch(ctx)
	defer sub.Close()

	var wg sync.WaitGroup
	for range goroutines {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for range eventsPerWriter {
				_ = s.Append(
					context.Background(),
					NewEvent("sess-race", Handoff, nil),
				)
			}
		}()
	}

	// Cancel mid-flight — should not panic.
	go func() {
		time.Sleep(2 * time.Millisecond)
		cancel()
	}()

	wg.Wait()
}

func TestWatch_EventsBeforeWatchNotReceived(t *testing.T) {
	s := New("sess-6")

	// Append before subscribe.
	_ = s.Append(context.Background(), NewEvent("sess-6", Handoff, nil))

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sub := s.Watch(ctx)
	defer sub.Close()

	// Append after subscribe.
	e := NewEvent("sess-6", Handoff, nil)
	_ = s.Append(context.Background(), e)

	select {
	case got := <-sub.Events():
		if got.ID != e.ID {
			t.Fatalf("got wrong event; want post-watch event")
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for post-watch event")
	}
}
