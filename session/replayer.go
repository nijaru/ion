package session

import (
	"fmt"
	"iter"
)

// Replayer reconstructs session state from an event stream.
type Replayer struct {
	reducer Reducer
}

// ReplayOption configures a Replayer.
type ReplayOption func(*Replayer)

// WithReplayReducer configures a reducer that is updated as events are replayed.
func WithReplayReducer(reducer Reducer) ReplayOption {
	return func(r *Replayer) {
		r.reducer = reducer
	}
}

// NewReplayer creates a concrete event-log replayer for session reconstruction.
func NewReplayer(opts ...ReplayOption) *Replayer {
	r := &Replayer{}
	for _, opt := range opts {
		opt(r)
	}
	return r
}

// NewSession creates an empty replay target session.
func (r *Replayer) NewSession(sessionID string) *Session {
	sess := New(sessionID)
	sess.reducer = r.reducer
	return sess
}

// Apply appends a replayed event to sess without triggering write-through or
// subscriber side effects.
func (r *Replayer) Apply(sess *Session, e Event) error {
	if sess == nil {
		return fmt.Errorf("replay: nil session")
	}
	if sess.id == "" {
		sess.id = e.SessionID
	}
	if e.SessionID != "" && sess.id != e.SessionID {
		return fmt.Errorf(
			"replay: session id mismatch: session=%q event=%q",
			sess.id,
			e.SessionID,
		)
	}

	sess.mu.Lock()
	defer sess.mu.Unlock()
	if e.ParentID == "" && sess.activeLeafID != "" {
		e.ParentID = sess.activeLeafID
	}
	if err := sess.validateTreeEventLocked(&e); err != nil {
		return err
	}
	sess.events = append(sess.events, e)
	sess.advanceReplaySequenceLocked(e)
	if sess.reducer != nil {
		sess.state = sess.reducer(sess.state, e)
	}
	if err := sess.advanceActiveLeafLocked(e); err != nil {
		return err
	}
	return nil
}

// Replay reconstructs a session from an event stream.
func (r *Replayer) Replay(sessionID string, events iter.Seq[Event]) (*Session, error) {
	sess := r.NewSession(sessionID)
	for e := range events {
		if err := r.Apply(sess, e); err != nil {
			return nil, err
		}
	}
	return sess, nil
}
