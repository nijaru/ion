package canto

import (
	"context"

	cantofw "github.com/nijaru/canto"
)

type turnState struct {
	seq           uint64
	active        bool
	accepted      bool
	cantoTurnID   string
	cancel        context.CancelFunc
	activeToolIDs map[string]struct{}
	canceled      map[uint64]struct{}
}

func newTurnState() turnState {
	return turnState{
		activeToolIDs: make(map[string]struct{}),
		canceled:      make(map[uint64]struct{}),
	}
}

func (s *turnState) start(cancel context.CancelFunc) uint64 {
	s.seq++
	s.active = true
	s.accepted = false
	s.cantoTurnID = ""
	s.cancel = cancel
	s.clearTools()
	return s.seq
}

func (s *turnState) accept(id uint64, cantoTurnID string) bool {
	if !s.activeFor(id) {
		return false
	}
	s.accepted = true
	s.cantoTurnID = cantoTurnID
	return true
}

func (s *turnState) finish(id uint64) bool {
	if s.seq == id && s.active {
		s.active = false
		s.accepted = false
		s.cantoTurnID = ""
		s.cancel = nil
		delete(s.canceled, id)
		s.clearTools()
		return true
	}
	if _, ok := s.canceled[id]; !ok {
		return false
	}
	delete(s.canceled, id)
	return id == s.seq && !s.active
}

func (s *turnState) requestCancel() (context.CancelFunc, bool) {
	if !s.active {
		return nil, false
	}
	if _, ok := s.canceled[s.seq]; ok {
		return nil, true
	}
	cancel := s.cancel
	s.cancel = nil
	if !s.accepted {
		s.active = false
	}
	if s.canceled == nil {
		s.canceled = make(map[uint64]struct{})
	}
	s.canceled[s.seq] = struct{}{}
	s.clearTools()
	return cancel, true
}

func (s *turnState) activeFor(id uint64) bool {
	return s.seq == id && s.active
}

func (s *turnState) cantoIDFor(id uint64) string {
	if !s.activeFor(id) {
		return ""
	}
	return s.cantoTurnID
}

func (s *turnState) accepts(id uint64) bool {
	if id == 0 || s.activeFor(id) {
		return true
	}
	_, ok := s.canceled[id]
	return ok
}

func (s *turnState) hasActiveTool() bool {
	return len(s.activeToolIDs) > 0
}

func (s *turnState) isCanceling(id uint64) bool {
	if id == 0 {
		return false
	}
	_, ok := s.canceled[id]
	return ok
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

func (s *turnState) markToolComplete(id uint64, toolID string) (bool, bool) {
	if toolID == "" || s.seq != id {
		return false, s.hasActiveTool()
	}
	if _, ok := s.activeToolIDs[toolID]; !ok {
		return false, s.hasActiveTool()
	}
	delete(s.activeToolIDs, toolID)
	return true, s.hasActiveTool()
}

func (s *turnState) setActiveTools(id uint64, tools []cantofw.RunToolLifecycle) {
	if !s.activeFor(id) {
		return
	}
	if s.activeToolIDs == nil {
		s.activeToolIDs = make(map[string]struct{})
	}
	s.clearTools()
	for _, tool := range tools {
		if tool.ID == "" {
			continue
		}
		s.activeToolIDs[tool.ID] = struct{}{}
	}
}

func (s *turnState) clearTools() {
	for id := range s.activeToolIDs {
		delete(s.activeToolIDs, id)
	}
}
