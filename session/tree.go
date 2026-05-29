package session

import (
	"context"
	"errors"
	"fmt"
	"slices"
)

var errSessionTreeCycle = errors.New("session tree: cycle detected")

// LeafMovedData records a durable active-branch movement.
type LeafMovedData struct {
	TargetEventID string `json:"target_event_id,omitzero"`
}

// NewLeafMovedEvent records that the active session branch moved to targetEventID.
// An empty target moves the active branch to the session root.
func NewLeafMovedEvent(sessionID string, targetEventID string) Event {
	return NewEvent(sessionID, LeafMoved, LeafMovedData{TargetEventID: targetEventID})
}

// LeafMovedData decodes the payload of a leaf-moved event.
func (e Event) LeafMovedData() (LeafMovedData, bool, error) {
	return decodeEventData[LeafMovedData](e, LeafMoved, "leaf moved")
}

// LeafID returns the event id at the tip of the active branch.
func (s *Session) LeafID() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.activeLeafID
}

// MoveLeaf records a durable active-branch movement. An empty eventID moves the
// active branch to the session root.
func (s *Session) MoveLeaf(ctx context.Context, eventID string) error {
	return s.Append(ctx, NewLeafMovedEvent(s.ID(), eventID))
}

// ActiveEvents returns the durable events on the active branch.
func (s *Session) ActiveEvents() ([]Event, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.activeEventsLocked()
}

// BranchEvents returns the durable events on the branch ending at eventID. An
// empty eventID returns an empty root branch.
func (s *Session) BranchEvents(eventID string) ([]Event, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.branchEventsLocked(eventID)
}

func (s *Session) activeEventsLocked() ([]Event, error) {
	return s.branchEventsLocked(s.activeLeafID)
}

func (s *Session) branchEventsLocked(eventID string) ([]Event, error) {
	if eventID == "" {
		return nil, nil
	}

	byID := make(map[string]Event, len(s.events))
	for _, e := range s.events {
		byID[e.ID.String()] = e
	}

	seen := make(map[string]struct{})
	events := make([]Event, 0, len(s.events))
	for currentID := eventID; currentID != ""; {
		if _, ok := seen[currentID]; ok {
			return nil, errSessionTreeCycle
		}
		seen[currentID] = struct{}{}

		e, ok := byID[currentID]
		if !ok {
			return nil, fmt.Errorf("session tree: event %q not found", currentID)
		}
		if e.Type != LeafMoved {
			events = append(events, e)
		}
		currentID = e.ParentID
	}
	slices.Reverse(events)
	return events, nil
}

func (s *Session) validateTreeEventLocked(e *Event) error {
	if e.ParentID != "" && !s.hasEventLocked(e.ParentID) {
		return fmt.Errorf("session tree: parent event %q not found", e.ParentID)
	}
	if e.Type != LeafMoved {
		return nil
	}
	data, _, err := e.LeafMovedData()
	if err != nil {
		return err
	}
	if data.TargetEventID == "" {
		return nil
	}
	if data.TargetEventID == e.ID.String() {
		return fmt.Errorf("session tree: leaf target cannot be the leaf event itself")
	}
	if !s.hasEventLocked(data.TargetEventID) {
		return fmt.Errorf("session tree: target event %q not found", data.TargetEventID)
	}
	return nil
}

func (s *Session) hasEventLocked(eventID string) bool {
	for _, event := range s.events {
		if event.ID.String() == eventID {
			return true
		}
	}
	return false
}

func (s *Session) advanceActiveLeafLocked(e Event) error {
	if e.Type != LeafMoved {
		s.activeLeafID = e.ID.String()
		return nil
	}
	data, _, err := e.LeafMovedData()
	if err != nil {
		return err
	}
	s.activeLeafID = data.TargetEventID
	return nil
}
