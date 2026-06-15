package session

import (
	"context"
	"fmt"
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

// validateTreeEventLocked checks that the event's parent exists in the event log.
func (s *Session) validateTreeEventLocked(e *Event) error {
	if e.ParentID == "" {
		return nil
	}
	for _, existing := range s.events {
		if existing.ID.String() == e.ParentID {
			return nil
		}
	}
	return fmt.Errorf("parent event %s does not exist", e.ParentID)
}

// advanceActiveLeafLocked updates the active leaf after appending an event.
func (s *Session) advanceActiveLeafLocked(e Event) error {
	if e.ParentID != "" && e.ParentID != s.activeLeafID {
		// Event is on a different branch - this is a leaf move
		if err := s.validateLeafMoveLocked(e.ParentID); err != nil {
			return err
		}
	}
	s.activeLeafID = e.ID.String()
	return nil
}

// validateLeafMoveLocked checks that the target is a valid leaf move.
func (s *Session) validateLeafMoveLocked(targetID string) error {
	// Any existing event can be a leaf target
	for _, existing := range s.events {
		if existing.ID.String() == targetID {
			return nil
		}
	}
	return fmt.Errorf("target event %s does not exist", targetID)
}

// activeEventsLocked returns events on the active path from root to leaf.
func (s *Session) activeEventsLocked() ([]Event, error) {
	if s.activeLeafID == "" {
		return nil, nil
	}

	// Build parent map
	parentMap := make(map[string]string)
	for _, e := range s.events {
		if e.ParentID != "" {
			parentMap[e.ID.String()] = e.ParentID
		}
	}

	// Trace path from leaf to root
	var path []string
	current := s.activeLeafID
	for current != "" {
		path = append(path, current)
		current = parentMap[current]
	}

	// Reverse to get root-first order
	for i, j := 0, len(path)-1; i < j; i, j = i+1, j-1 {
		path[i], path[j] = path[j], path[i]
	}

	// Collect events in path order
	result := make([]Event, 0, len(path))
	for _, id := range path {
		for _, e := range s.events {
			if e.ID.String() == id {
				result = append(result, e)
				break
			}
		}
	}
	return result, nil
}

// MoveLeaf moves the active leaf to the specified event ID.
func (s *Session) MoveLeaf(ctx context.Context, targetID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := s.validateLeafMoveLocked(targetID); err != nil {
		return err
	}

	s.activeLeafID = targetID
	return nil
}

// ActiveLeafID returns the current active leaf event ID.
func (s *Session) ActiveLeafID() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.activeLeafID
}
