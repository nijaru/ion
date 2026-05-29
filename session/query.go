package session

import (
	"iter"
	"log/slog"

	"github.com/nijaru/ion/llm"
)

// Events returns the full event log.
func (s *Session) Events() []Event {
	s.mu.RLock()
	defer s.mu.RUnlock()
	res := make([]Event, len(s.events))
	copy(res, s.events)
	for i := range res {
		if err := res[i].ensureMetadata(); err != nil {
			slog.Warn("event metadata decode failed", "event_id", res[i].ID, "error", err)
		}
	}
	return res
}

func (s *Session) snapshotEvents() []Event {
	s.mu.RLock()
	defer s.mu.RUnlock()
	res := make([]Event, len(s.events))
	copy(res, s.events)
	return res
}

// All returns an iterator over the full event log from oldest to newest.
func (s *Session) All() iter.Seq[Event] {
	return func(yield func(Event) bool) {
		for _, e := range s.snapshotEvents() {
			if !yield(e) {
				return
			}
		}
	}
}

// Backward returns an iterator over the full event log from newest to oldest.
func (s *Session) Backward() iter.Seq[Event] {
	return func(yield func(Event) bool) {
		events := s.snapshotEvents()
		for i := len(events) - 1; i >= 0; i-- {
			if !yield(events[i]) {
				return
			}
		}
	}
}

// Messages extracts all messages from the event log.
func (s *Session) Messages() []llm.Message {
	s.mu.Lock()
	defer s.mu.Unlock()

	var res []llm.Message
	for i := range s.events {
		if s.events[i].Type == MessageAdded {
			m, err := s.events[i].ensureMessage()
			if err == nil {
				res = append(res, *m)
			}
		}
	}
	return res
}

// TotalCost returns the sum of costs across all events in the session.
func (s *Session) TotalCost() float64 {
	s.mu.RLock()
	defer s.mu.RUnlock()
	var total float64
	for _, e := range s.events {
		total += e.Cost
	}
	return total
}

// IsWaiting returns true if the session is currently waiting for external input
// or approval (HITL).
func (s *Session) IsWaiting() bool {
	s.mu.RLock()
	defer s.mu.RUnlock()

	for i := len(s.events) - 1; i >= 0; i-- {
		e := s.events[i]
		switch e.Type {
		case WaitStarted, ApprovalRequested:
			return true
		case WaitResolved, ApprovalResolved, ApprovalCanceled:
			return false
		}
	}
	return false
}

// LastEvent returns the most recent event in the session, if any.
func (s *Session) LastEvent() (Event, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if len(s.events) == 0 {
		return Event{}, false
	}
	return s.events[len(s.events)-1], true
}

// LastMessage returns the most recent message in the session, if any.
func (s *Session) LastMessage() (llm.Message, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	for i := len(s.events) - 1; i >= 0; i-- {
		e := &s.events[i]
		if e.Type == MessageAdded {
			m, err := e.ensureMessage()
			if err == nil {
				return *m, true
			}
		}
	}
	return llm.Message{}, false
}

// LastAssistantMessage returns the most recent assistant message without tool
// calls in the session, if any.
func (s *Session) LastAssistantMessage() (llm.Message, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	for i := len(s.events) - 1; i >= 0; i-- {
		e := &s.events[i]
		if e.Type == MessageAdded {
			m, err := e.ensureMessage()
			if err == nil && m.Role == llm.RoleAssistant && len(m.Calls) == 0 &&
				validModelMessage(*m) {
				return *m, true
			}
		}
	}
	return llm.Message{}, false
}
