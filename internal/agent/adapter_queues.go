package agent

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/session"
)

// SteerTurn sends steering input during an active turn.
func (s *SessionAdapter) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return session.SteeringResult{}, fmt.Errorf("session is closed")
	}
	s.steeringQueue = append(s.steeringQueue, text)
	s.emitQueueUpdatedLocked()
	s.mu.Unlock()

	return session.SteeringResult{
		Outcome: session.SteeringAccepted,
		Notice:  "Steering input accepted",
	}, nil
}

// FollowUpTurn sends follow-up input after the agent would stop.
func (s *SessionAdapter) FollowUpTurn(
	ctx context.Context,
	text string,
) (session.QueuedInputResult, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return session.QueuedInputResult{}, fmt.Errorf("session is closed")
	}
	s.followUpQueue = append(s.followUpQueue, text)
	s.emitQueueUpdatedLocked()
	s.mu.Unlock()

	return session.QueuedInputResult{
		Outcome: session.QueuedInputAccepted,
		Notice:  "Follow-up input accepted",
	}, nil
}

// ClearQueuedInput clears queued input and returns the snapshot.
func (s *SessionAdapter) ClearQueuedInput(
	ctx context.Context,
) (session.QueuedInputSnapshot, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return session.QueuedInputSnapshot{}, fmt.Errorf("session is closed")
	}

	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), s.steeringQueue...),
		FollowUp: append([]string(nil), s.followUpQueue...),
	}

	s.steeringQueue = nil
	s.followUpQueue = nil
	s.emitQueueUpdatedLocked()

	return snapshot, nil
}

func drainQueuedMessagesLocked(queue *[]string, mode QueueMode) []AgentMessage {
	if len(*queue) == 0 {
		return nil
	}
	count := 1
	if mode == QueueModeAll {
		count = len(*queue)
	}
	msgs := make([]AgentMessage, count)
	for i, text := range (*queue)[:count] {
		msgs[i] = AgentMessage{Role: "user", Content: text}
	}
	*queue = (*queue)[count:]
	return msgs
}

func (s *SessionAdapter) emitQueueUpdatedLocked() {
	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), s.steeringQueue...),
		FollowUp: append([]string(nil), s.followUpQueue...),
	}
	s.emitEvent(session.QueuedInputUpdate{
		Base:     session.BaseNow(),
		Snapshot: snapshot,
	})
}
