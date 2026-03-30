package tools

import (
	"sync"
)

// ApprovalManager handles the synchronization between the agent and the host
// for tools that require explicit user approval.
type ApprovalManager struct {
	mu       sync.Mutex
	requests map[string]chan bool
}

func NewApprovalManager() *ApprovalManager {
	return &ApprovalManager{
		requests: make(map[string]chan bool),
	}
}

func (m *ApprovalManager) Request(id string) chan bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	ch := make(chan bool, 1)
	m.requests[id] = ch
	return ch
}

func (m *ApprovalManager) Remove(id string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.requests, id)
}

func (m *ApprovalManager) Approve(id string, approved bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if ch, ok := m.requests[id]; ok {
		ch <- approved
		delete(m.requests, id)
	}
}
