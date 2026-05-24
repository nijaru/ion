package canto

import (
	"context"
)

type turnState struct {
	seq           uint64
	active        bool
	accepted      bool
	cantoTurnID   string
	cancel        context.CancelFunc
	canceled      map[uint64]struct{}
	terminal      bool
	settled       bool
	terminalError bool
}

func newTurnState() turnState {
	return turnState{
		canceled: make(map[uint64]struct{}),
	}
}

func (s *turnState) start(cancel context.CancelFunc) uint64 {
	s.seq++
	s.active = true
	s.accepted = false
	s.cantoTurnID = ""
	s.cancel = cancel
	s.terminal = false
	s.settled = false
	s.terminalError = false
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
		s.terminal = false
		s.settled = false
		s.terminalError = false
		delete(s.canceled, id)
		return true
	}
	if _, ok := s.canceled[id]; !ok {
		return false
	}
	delete(s.canceled, id)
	return id == s.seq && !s.active
}

func (s *turnState) finishCanto(cantoTurnID string) bool {
	if cantoTurnID == "" || !s.active || s.cantoTurnID != cantoTurnID {
		return false
	}
	s.settled = true
	if !s.terminal {
		return false
	}
	return s.finish(s.seq)
}

func (s *turnState) markTerminal(id uint64) bool {
	if id != 0 && !s.activeFor(id) {
		return false
	}
	s.terminal = true
	if !s.settled {
		return false
	}
	return s.finish(s.seq)
}

func (s *turnState) markTerminalError(id uint64) (emitError bool, finish bool) {
	if id != 0 && !s.activeFor(id) {
		return false, false
	}
	if s.terminalError {
		return false, false
	}
	s.terminal = true
	s.terminalError = true
	if s.settled {
		return true, s.finish(s.seq)
	}
	return true, false
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

func (s *turnState) isCanceling(id uint64) bool {
	if id == 0 {
		return false
	}
	_, ok := s.canceled[id]
	return ok
}
