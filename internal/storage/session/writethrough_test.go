package session

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/oklog/ulid/v2"
)

// memStore is an in-memory Store for testing.
type memStore struct {
	mu     sync.Mutex
	events []Event
}

func (m *memStore) Save(_ context.Context, e Event) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.events = append(m.events, e)
	return nil
}

func (m *memStore) Load(_ context.Context, _ string) (*Session, error) { return nil, nil }

func (m *memStore) LoadUntil(_ context.Context, _ string, _ ulid.ULID) (*Session, error) {
	return nil, nil
}

func (m *memStore) Fork(_ context.Context, _, _ string) (*Session, error) {
	return nil, nil
}

func (m *memStore) saved() []Event {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := make([]Event, len(m.events))
	copy(out, m.events)
	return out
}

type blockingStore struct {
	mu      sync.Mutex
	events  []Event
	started sync.Once
	startCh chan struct{}
	release chan struct{}
}

func newBlockingStore() *blockingStore {
	return &blockingStore{
		startCh: make(chan struct{}),
		release: make(chan struct{}),
	}
}

func (s *blockingStore) Save(_ context.Context, e Event) error {
	s.started.Do(func() {
		close(s.startCh)
	})
	<-s.release
	s.mu.Lock()
	defer s.mu.Unlock()
	s.events = append(s.events, e)
	return nil
}

func (s *blockingStore) Load(_ context.Context, _ string) (*Session, error) { return nil, nil }

func (s *blockingStore) LoadUntil(_ context.Context, _ string, _ ulid.ULID) (*Session, error) {
	return nil, nil
}

func (s *blockingStore) Fork(_ context.Context, _, _ string) (*Session, error) {
	return nil, nil
}

func (s *blockingStore) saved() []Event {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]Event, len(s.events))
	copy(out, s.events)
	return out
}

func TestAttachWriteThrough_SavesEvents(t *testing.T) {
	sess := New("wt-1")
	store := &memStore{}

	cancel := AttachWriteThrough(context.Background(), sess, store)
	defer cancel()

	_ = sess.Append(context.Background(), NewEvent("wt-1", Handoff, nil))
	_ = sess.Append(context.Background(), NewEvent("wt-1", Handoff, nil))

	// Give the goroutine time to save.
	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		if len(store.saved()) == 2 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}

	saved := store.saved()
	if len(saved) != 2 {
		t.Fatalf("saved %d events, want 2", len(saved))
	}
}

func TestAttachWriteThrough_CancelDrainsAcceptedEvents(t *testing.T) {
	sess := New("wt-drain")
	store := newBlockingStore()

	cancel := AttachWriteThrough(context.Background(), sess, store)
	if err := sess.Append(context.Background(), NewEvent("wt-drain", Handoff, nil)); err != nil {
		t.Fatalf("append first: %v", err)
	}
	select {
	case <-store.startCh:
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for first save")
	}
	if err := sess.Append(context.Background(), NewEvent("wt-drain", Handoff, nil)); err != nil {
		t.Fatalf("append second: %v", err)
	}

	done := make(chan struct{})
	go func() {
		cancel()
		close(done)
	}()

	select {
	case <-done:
		t.Fatal("cancel returned before accepted events drained")
	case <-time.After(20 * time.Millisecond):
	}

	close(store.release)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for cancel")
	}

	saved := store.saved()
	if len(saved) != 2 {
		t.Fatalf("saved %d events, want 2", len(saved))
	}
}

func TestAttachWriteThrough_CancelStops(t *testing.T) {
	sess := New("wt-2")
	store := &memStore{}

	cancel := AttachWriteThrough(context.Background(), sess, store)
	cancel() // detach immediately

	// Append after cancel — should NOT be saved.
	time.Sleep(20 * time.Millisecond)
	_ = sess.Append(context.Background(), NewEvent("wt-2", Handoff, nil))
	time.Sleep(20 * time.Millisecond)

	if n := len(store.saved()); n != 0 {
		t.Fatalf("saved %d events after cancel, want 0", n)
	}
}

func TestAttachWriteThrough_EventsBeforeAttachNotSaved(t *testing.T) {
	sess := New("wt-3")
	store := &memStore{}

	// Append before attaching.
	_ = sess.Append(context.Background(), NewEvent("wt-3", Handoff, nil))

	cancel := AttachWriteThrough(context.Background(), sess, store)
	defer cancel()

	// Append after attaching.
	_ = sess.Append(context.Background(), NewEvent("wt-3", Handoff, nil))

	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		if len(store.saved()) == 1 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}

	saved := store.saved()
	if len(saved) != 1 {
		t.Fatalf("saved %d events, want 1 (only post-attach)", len(saved))
	}
	if saved[0].Type != Handoff {
		t.Fatalf("saved event type = %q, want handoff", saved[0].Type)
	}
}

func TestAttachWriteThrough_CancelDuringConcurrentAppendDoesNotPanic(t *testing.T) {
	sess := New("wt-race")
	store := &memStore{}

	cancel := AttachWriteThrough(context.Background(), sess, store)

	var wg sync.WaitGroup
	for range 16 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for range 50 {
				_ = sess.Append(context.Background(), NewEvent("wt-race", Handoff, nil))
			}
		}()
	}

	time.Sleep(5 * time.Millisecond)
	cancel()
	wg.Wait()
}

func TestAttachWriteThrough_CancelDoesNotSaveZeroEvent(t *testing.T) {
	sess := New("wt-zero")
	store := &memStore{}

	cancel := AttachWriteThrough(context.Background(), sess, store)
	if err := sess.Append(context.Background(), NewEvent("wt-zero", Handoff, nil)); err != nil {
		t.Fatalf("append: %v", err)
	}
	cancel()

	for _, event := range store.saved() {
		if event.SessionID == "" || event.Type == "" {
			t.Fatalf("saved zero event after cancel: %#v", event)
		}
	}
}
