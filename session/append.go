package session

import (
	"context"
	"errors"

	"github.com/nijaru/ion/llm"
)

var errEmptyAssistantMessage = errors.New(
	"session append: assistant message has no content, reasoning, thinking blocks, or tool calls",
)

var errInvalidMessageRole = errors.New("session append: message has invalid role")

var errUnmatchedToolMessage = errors.New(
	"session append: tool message has no matching pending assistant tool call",
)

var errInvalidEventSequence = errors.New("session append: event sequence cannot be negative")

var errNonMonotonicEventSequence = errors.New("session append: event sequence is not monotonic")

// Append adds a new event to the session and notifies all subscribers.
// If a writer is attached, the event is persisted to the store immediately.
// If the context contains metadata, it is merged into the event's metadata.
func (s *Session) Append(ctx context.Context, e Event) error {
	if err := validateWritableEvent(&e); err != nil {
		return err
	}

	if md := MetadataFromContext(ctx); len(md) > 0 {
		newMd := make(map[string]any, len(e.Metadata)+len(md))
		if e.Metadata != nil {
			for k, v := range e.Metadata {
				newMd[k] = v
			}
		}
		for k, v := range md {
			if _, exists := newMd[k]; !exists {
				newMd[k] = v
			}
		}
		e.Metadata = newMd
	}

	s.mu.Lock()
	if err := s.validateWritableSequenceLocked(&e); err != nil {
		s.mu.Unlock()
		return err
	}
	if err := s.assignEventIdentityLocked(ctx, &e); err != nil {
		s.mu.Unlock()
		return err
	}
	if err := s.validateTreeEventLocked(&e); err != nil {
		s.mu.Unlock()
		return err
	}
	writer := s.writer
	writerCh := s.writerCh

	if writer != nil {
		if err := writer.Save(ctx, e); err != nil {
			s.mu.Unlock()
			return err
		}
	}

	if writerCh != nil {
		if err := writerCh.send(ctx, e); err != nil {
			s.mu.Unlock()
			return err
		}
	}

	s.events = append(s.events, e)
	if s.reducer != nil {
		s.state = s.reducer(s.state, e)
	}
	if err := s.advanceActiveLeafLocked(e); err != nil {
		s.mu.Unlock()
		return err
	}
	subs := append([]*subscriber(nil), s.subscribers...)
	observerErr := s.notifyObserversLocked(ctx, e)
	s.mu.Unlock()

	for _, sub := range subs {
		sub.trySend(e)
	}
	return observerErr
}

func (s *Session) assignEventIdentityLocked(ctx context.Context, e *Event) error {
	if e.Seq < 0 {
		return errInvalidEventSequence
	}
	if s.nextSeq == 0 {
		s.nextSeq = int64(len(s.events) + 1)
	}
	if e.Seq == 0 {
		e.Seq = s.nextSeq
		s.nextSeq++
	} else if e.Seq < s.nextSeq {
		return errNonMonotonicEventSequence
	} else if e.Seq >= s.nextSeq {
		s.nextSeq = e.Seq + 1
	}
	if e.TurnID == "" {
		e.TurnID = TurnIDFromContext(ctx)
	}
	if e.ParentID == "" && s.activeLeafID != "" {
		e.ParentID = s.activeLeafID
	}
	return nil
}

func (s *Session) advanceReplaySequenceLocked(e Event) {
	if e.Seq >= s.nextSeq {
		s.nextSeq = e.Seq + 1
		return
	}
	if e.Seq == 0 && s.nextSeq <= int64(len(s.events)) {
		s.nextSeq = int64(len(s.events) + 1)
	}
}

func (s *Session) validateWritableSequenceLocked(e *Event) error {
	if e.Type != MessageAdded {
		return nil
	}
	msg, err := e.ensureMessage()
	if err != nil {
		return err
	}
	if msg.Role != llm.RoleTool {
		return nil
	}
	if msg.ToolID == "" {
		return errUnmatchedToolMessage
	}
	activeEvents, err := s.activeEventsLocked()
	if err != nil {
		return err
	}
	pending, err := pendingToolCalls(activeEvents)
	if err != nil {
		return err
	}
	if pending[msg.ToolID] == 0 {
		return errUnmatchedToolMessage
	}
	return nil
}

func validateWritableEvent(e *Event) error {
	if e.Type == BranchSummary {
		summary, ok, err := e.BranchSummaryData()
		if err != nil {
			return err
		}
		if ok {
			return validateBranchSummary(summary)
		}
	}
	if e.Type != MessageAdded {
		return nil
	}
	msg, err := e.ensureMessage()
	if err != nil {
		return err
	}
	return validateModelMessage(*msg)
}

func validateModelMessage(msg llm.Message) error {
	switch msg.Role {
	case llm.RoleUser, llm.RoleAssistant, llm.RoleTool:
		// Valid conversation roles
	default:
		// System and developer roles are not valid for session messages.
		// Use Session.SetSystemPrompt() for system instructions.
		return errInvalidMessageRole
	}
	if msg.Role == llm.RoleAssistant && !assistantMessageHasPayload(msg) {
		return errEmptyAssistantMessage
	}
	return nil
}
