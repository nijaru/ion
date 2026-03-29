package tools

import (
	"context"
	"fmt"
	"sync"

	"github.com/oklog/ulid/v2"
	"github.com/nijaru/canto/tool"
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

// ApprovingTool wraps a standard canto.Tool and intercepts execution
// to request host approval via a callback.
type ApprovingTool struct {
	tool.Tool
	Manager  *ApprovalManager
	Callback func(id, description string)
}

func (t *ApprovingTool) Execute(ctx context.Context, args string) (string, error) {
	id := ulid.Make().String()
	description := fmt.Sprintf("Tool: %s\nArgs: %s", t.Spec().Name, args)

	// Send approval request to host
	t.Callback(id, description)

	// Wait for approval
	ch := t.Manager.Request(id)
	defer t.Manager.Remove(id)
	select {
	case <-ctx.Done():
		return "", ctx.Err()
	case approved := <-ch:
		if !approved {
			return "User denied tool execution.", nil
		}
	}

	// Proceed with execution if approved
	return t.Tool.Execute(ctx, args)
}
