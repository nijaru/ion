package app

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/session"
)

func TestApprovalFlow(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event, 10)}
	b := &stubBackend{sess: sess}
	m := New(b, nil, nil, "/tmp", "main", "dev", nil)

	// 1. Receive ApprovalRequest
	req := session.ApprovalRequest{
		RequestID:   "req-1",
		Description: "Run rm -rf /",
		ToolName:    "bash",
	}
	updated, _ := m.Update(req)
	m = updated.(Model)

	if m.Approval.Pending == nil || m.Approval.Pending.RequestID != "req-1" {
		t.Fatal("expected pending approval req-1")
	}
	if m.Progress.Mode != stateApproval {
		t.Fatalf("expected stateApproval, got %v", m.Progress.Mode)
	}

	// 2. Approve with 'y'
	updated, _ = m.Update(tea.KeyPressMsg{Code: 'y', Text: "y"})
	m = updated.(Model)

	if m.Approval.Pending != nil {
		t.Fatal("expected approval to be cleared after 'y'")
	}
	if m.Progress.Mode != stateReady {
		t.Fatalf("expected stateReady after approval, got %v", m.Progress.Mode)
	}

	// 3. Deny with 'n'
	m.Approval.Pending = &req
	m.Progress.Mode = stateApproval
	updated, _ = m.Update(tea.KeyPressMsg{Code: 'n', Text: "n"})
	m = updated.(Model)

	if m.Approval.Pending != nil {
		t.Fatal("expected approval to be cleared after 'n'")
	}

	// 4. Always allow with 'a'
	m.Approval.Pending = &req
	m.Progress.Mode = stateApproval
	updated, _ = m.Update(tea.KeyPressMsg{Code: 'a', Text: "a"})
	m = updated.(Model)

	if m.Approval.Pending != nil {
		t.Fatal("expected approval to be cleared after 'a'")
	}
}

func TestToolStreaming(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event, 10)}
	b := &stubBackend{sess: sess}
	m := New(b, nil, nil, "/tmp", "main", "dev", nil)

	// 1. Tool started
	updated, _ := m.Update(session.ToolCallStarted{
		ToolName: "bash",
		Args:     "ls",
	})
	m = updated.(Model)

	if m.InFlight.Pending == nil || m.InFlight.Pending.Role != session.Tool {
		t.Fatal("expected pending tool entry")
	}

	// 2. Output delta
	updated, _ = m.Update(session.ToolOutputDelta{Delta: "file1.txt\n"})
	m = updated.(Model)

	if m.InFlight.Pending.Content != "file1.txt\n" {
		t.Fatalf("expected content 'file1.txt\n', got %q", m.InFlight.Pending.Content)
	}

	// 3. Tool result
	updated, cmd := m.Update(session.ToolResult{
		ToolName: "bash",
		Result:   "file1.txt\nfile2.txt\n",
	})
	m = updated.(Model)

	if m.InFlight.Pending != nil {
		t.Fatal("expected pending entry cleared after ToolResult")
	}
	if cmd == nil {
		t.Fatal("expected print cmd after ToolResult")
	}
}

func TestRenderEntry(t *testing.T) {
	b := &stubBackend{}
	m := New(b, nil, nil, "/tmp", "main", "dev", nil)

	// 1. Agent message with multiple lines
	entry := session.Entry{
		Role:    session.Agent,
		Content: "Line 1\n\nLine 2\n\nLine 3",
	}
	rendered := m.renderEntry(entry)
	expected := "• Line 1\n\nLine 2\n\nLine 3"
	// Strip ansi for comparison if needed, but here we expect plain text + bullet
	if ansi.Strip(rendered) != expected {
		t.Errorf("expected:\n%q\ngot:\n%q", expected, ansi.Strip(rendered))
	}

	// 2. Agent message with reasoning
	entry = session.Entry{
		Role:      session.Agent,
		Reasoning: "Thought 1",
		Content:   "Reply 1",
	}
	rendered = m.renderEntry(entry)
	if !strings.Contains(ansi.Strip(rendered), "Thinking") {
		t.Error("expected 'Thinking' in output")
	}
	if strings.Contains(ansi.Strip(rendered), "Thought 1") {
		t.Error("reasoning should be collapsed by default")
	}
	if !strings.Contains(ansi.Strip(rendered), "Reply 1") {
		t.Error("expected 'Reply 1' in output")
	}
}

func TestAsyncSubagents(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event, 10)}
	b := &stubBackend{sess: sess}
	m := New(b, nil, nil, "/tmp", "main", "dev", nil)

	// 1. Worker 1 requested
	updated, _ := m.Update(session.ChildRequested{
		AgentName: "worker-1",
		Query:     "task 1",
	})
	m = updated.(Model)

	// 2. Worker 2 requested
	updated, _ = m.Update(session.ChildRequested{
		AgentName: "worker-2",
		Query:     "task 2",
	})
	m = updated.(Model)

	if len(m.InFlight.Subagents) != 2 {
		t.Fatalf("expected 2 subagents, got %d", len(m.InFlight.Subagents))
	}

	// 3. Worker 1 progresses
	updated, _ = m.Update(session.ChildDelta{
		AgentName: "worker-1",
		Delta:     "working on 1...",
	})
	m = updated.(Model)

	// 4. Worker 2 progresses
	updated, _ = m.Update(session.ChildDelta{
		AgentName: "worker-2",
		Delta:     "working on 2...",
	})
	m = updated.(Model)

	if !strings.Contains(m.InFlight.Subagents["worker-1"].Output, "working on 1...") {
		t.Error("worker-1 output missing progress")
	}
	if !strings.Contains(m.InFlight.Subagents["worker-2"].Output, "working on 2...") {
		t.Error("worker-2 output missing progress")
	}

	// 5. Worker 1 completes
	updated, _ = m.Update(session.ChildCompleted{
		AgentName: "worker-1",
		Result:    "result 1",
	})
	m = updated.(Model)

	if _, ok := m.InFlight.Subagents["worker-1"]; ok {
		t.Error("worker-1 should be removed from map after completion")
	}
	if _, ok := m.InFlight.Subagents["worker-2"]; !ok {
		t.Error("worker-2 should still be in map")
	}
}

func TestSubagentCollapseRule(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event, 10)}
	b := &stubBackend{sess: sess}
	m := New(b, nil, nil, "/tmp", "main", "dev", nil)

	// 1. Request 5 workers
	for i := 1; i <= 5; i++ {
		name := fmt.Sprintf("worker-%d", i)
		updated, _ := m.Update(session.ChildRequested{
			AgentName: name,
			Query:     "task",
		})
		m = updated.(Model)
	}

	if len(m.InFlight.Subagents) != 5 {
		t.Fatalf("expected 5 subagents, got %d", len(m.InFlight.Subagents))
	}

	// 2. Render Plane B
	planeB := ansi.Strip(m.renderPlaneB())

	// We expect 3 workers and a "+2 more workers" line
	if !strings.Contains(planeB, "worker-1") || !strings.Contains(planeB, "worker-2") || !strings.Contains(planeB, "worker-3") {
		t.Error("expected first 3 workers to be visible")
	}
	if strings.Contains(planeB, "worker-4") || strings.Contains(planeB, "worker-5") {
		t.Error("expected workers 4 and 5 to be collapsed")
	}
	if !strings.Contains(planeB, "+2 more workers") {
		t.Errorf("expected collapse notice, got:\n%s", planeB)
	}
}
