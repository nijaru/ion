package session

import (
	"context"
	"sync"
)

// EventObserver receives newly appended events synchronously from Append.
//
// Observers are ordered and backpressured: Append does not return until every
// attached observer has accepted the event. Observer callbacks must not reenter
// the same session.
type EventObserver func(context.Context, Event) error

type eventObserver struct {
	fn EventObserver
}

// ObserveEvents attaches fn as an ordered, non-lossy observer for future
// appends. The returned function detaches the observer and is safe to call more
// than once.
func (s *Session) ObserveEvents(fn EventObserver) func() {
	if s == nil || fn == nil {
		return func() {}
	}

	obs := &eventObserver{fn: fn}
	s.mu.Lock()
	s.observers = append(s.observers, obs)
	s.mu.Unlock()

	var once sync.Once
	return func() {
		once.Do(func() {
			s.removeObserver(obs)
		})
	}
}

func (s *Session) removeObserver(target *eventObserver) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i, obs := range s.observers {
		if obs == target {
			s.observers = append(s.observers[:i], s.observers[i+1:]...)
			return
		}
	}
}

func (s *Session) notifyObserversLocked(ctx context.Context, e Event) error {
	for _, obs := range s.observers {
		if err := obs.fn(ctx, e); err != nil {
			return err
		}
	}
	return nil
}
