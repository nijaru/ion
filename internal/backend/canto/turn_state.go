package canto

import "context"

type turnState struct {
	seq           uint64
	active        bool
	cancel        context.CancelFunc
	activeToolIDs map[string]struct{}
}

func newTurnState() turnState {
	return turnState{activeToolIDs: make(map[string]struct{})}
}

func (s *turnState) start(cancel context.CancelFunc) uint64 {
	s.seq++
	s.active = true
	s.cancel = cancel
	s.clearTools()
	return s.seq
}

func (s *turnState) finish(id uint64) bool {
	if s.seq != id || !s.active {
		return false
	}
	s.active = false
	s.cancel = nil
	s.clearTools()
	return true
}

func (s *turnState) cancelActive() (context.CancelFunc, bool) {
	cancel := s.cancel
	active := s.active
	s.cancel = nil
	s.active = false
	s.clearTools()
	return cancel, active
}

func (s *turnState) activeFor(id uint64) bool {
	return s.seq == id && s.active
}

func (s *turnState) accepts(id uint64) bool {
	return id == 0 || s.activeFor(id)
}

func (s *turnState) hasActiveTool() bool {
	return len(s.activeToolIDs) > 0
}

func (s *turnState) markToolActive(id uint64, toolID string) {
	if toolID == "" || !s.activeFor(id) {
		return
	}
	if s.activeToolIDs == nil {
		s.activeToolIDs = make(map[string]struct{})
	}
	s.activeToolIDs[toolID] = struct{}{}
}

func (s *turnState) markToolComplete(id uint64, toolID string) {
	if toolID == "" || s.seq != id {
		return
	}
	delete(s.activeToolIDs, toolID)
}

func (s *turnState) clearTools() {
	for id := range s.activeToolIDs {
		delete(s.activeToolIDs, id)
	}
}
