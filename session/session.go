package session

import (
	"context"
	"sync"
)

// Writer persists events to a durable store.
type Writer interface {
	Save(ctx context.Context, e Event) error
}

// Reducer computes a state snapshot from a sequence of events.
type Reducer func(state map[string]any, e Event) map[string]any

// Session is a durable container for a conversation.
// All state is derived from an append-only event log.
//
// System prompts are stored separately from conversation events.
// They are added to provider requests at request time based on the
// model's capabilities (e.g., system vs developer role).
type Session struct {
	mu           sync.RWMutex
	id           string
	systemPrompt string
	events       []Event
	activeLeafID string
	nextSeq      int64
	state        map[string]any
	subscribers  []*subscriber
	observers    []*eventObserver
	writer       Writer
	writerCh     *writerChannel
	reducer      Reducer
}

// New creates a new session.
func New(id string) *Session {
	return &Session{
		id:      id,
		nextSeq: 1,
		state:   make(map[string]any),
	}
}

// WithReducer attaches a reducer to the session for state management.
func (s *Session) WithReducer(r Reducer) *Session {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.reducer = r
	// Recompute state from existing events
	s.state = make(map[string]any)
	for _, e := range s.events {
		s.state = r(s.state, e)
	}
	return s
}

// State returns a snapshot of the current session state.
func (s *Session) State() map[string]any {
	s.mu.RLock()
	defer s.mu.RUnlock()
	res := make(map[string]any, len(s.state))
	for k, v := range s.state {
		res[k] = v
	}
	return res
}

// WithWriter attaches a writer to the session for write-through persistence.
func (s *Session) WithWriter(w Writer) *Session {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.writer = w
	return s
}

// ID returns the session identifier.
func (s *Session) ID() string {
	return s.id
}

// SetSystemPrompt sets the system prompt for the session.
// The system prompt is not stored as a conversation event; it is added
// to provider requests at request time based on the model's capabilities.
func (s *Session) SetSystemPrompt(prompt string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.systemPrompt = prompt
}

// SystemPrompt returns the session's system prompt.
func (s *Session) SystemPrompt() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.systemPrompt
}

func (s *Session) setWriterChannel(ch chan<- Event) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.writerCh = &writerChannel{ch: ch}
}

func (s *Session) unsetWriterChannel() {
	s.mu.Lock()
	writerCh := s.writerCh
	s.writerCh = nil
	s.mu.Unlock()

	if writerCh != nil {
		writerCh.close()
	}
}

func (s *Session) removeSubscriber(target *subscriber) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i, sub := range s.subscribers {
		if sub == target {
			s.subscribers = append(s.subscribers[:i], s.subscribers[i+1:]...)
			return
		}
	}
}
