package app

import (
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestTurnReducerClearActiveStateCanKeepQueuedTurns(t *testing.T) {
	model := readyModel(t)
	tool := &session.Entry{Role: session.RoleTool, Content: "partial"}
	model.InFlight.Thinking = true
	model.InFlight.Pending = tool
	model.InFlight.PendingTools = map[string]*session.Entry{"tool-a": tool}
	model.InFlight.Subagents = map[string]*SubagentProgress{"child": {ID: "child"}}
	model.InFlight.QueuedTurns = []string{"follow up"}
	model.InFlight.StreamBuf = "stream"
	model.InFlight.ReasonBuf = "reason"
	model.InFlight.AgentCommitted = true
	model.InFlight.DrainUntilTurnStarted = true
	model.InFlight.DrainStartedAt = time.Now()
	model.Progress.LastToolUseID = "tool-a"
	model.Progress.ContextTokens = 123

	model.turnReducer().clearActiveState(false)

	if model.InFlight.Thinking ||
		model.InFlight.Pending != nil ||
		model.InFlight.PendingTools != nil ||
		model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" ||
		model.InFlight.AgentCommitted ||
		model.InFlight.DrainUntilTurnStarted ||
		!model.InFlight.DrainStartedAt.IsZero() ||
		model.Progress.LastToolUseID != "" ||
		model.Progress.ContextTokens != 0 {
		t.Fatalf("active state not cleared: %#v progress=%#v", model.InFlight, model.Progress)
	}
	if len(model.InFlight.Subagents) != 0 {
		t.Fatalf("subagents = %#v, want reset empty map", model.InFlight.Subagents)
	}
	if len(model.InFlight.QueuedTurns) != 1 || model.InFlight.QueuedTurns[0] != "follow up" {
		t.Fatalf("queued turns = %#v, want preserved follow-up", model.InFlight.QueuedTurns)
	}

	model.turnReducer().clearActiveState(true)
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want cleared", model.InFlight.QueuedTurns)
	}
}

func TestTurnReducerFinishesPendingAssistantFromStream(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Pending = &session.Entry{
		Role:    session.RoleAgent,
		Content: "answer",
	}
	model.InFlight.StreamBuf = "answer"
	model.InFlight.ReasonBuf = "reasoning"

	entry, completed, ok := model.turnReducer().finishPendingAssistant()
	if !ok {
		t.Fatal("finishPendingAssistant did not return pending stream entry")
	}
	if !completed {
		t.Fatal("assistantCompleted = false, want true")
	}
	if entry.Content != "answer" || entry.Reasoning != "reasoning" {
		t.Fatalf("entry = %#v, want streamed answer with reasoning", entry)
	}
	if model.InFlight.Pending != nil ||
		model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" {
		t.Fatalf("pending stream state not cleared: %#v", model.InFlight)
	}
}

func TestTurnReducerFinishModeClearsStaleStateOnEmptyAssistant(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateWorking
	model.Progress.Status = "Running bash..."
	model.Progress.LastError = ""
	model.InFlight.Thinking = true
	model.InFlight.QueuedTurns = []string{"stale follow-up"}
	model.InFlight.Pending = &session.Entry{Role: session.RoleAgent}

	entry, ok := model.turnReducer().finishTurnMode(false)
	if !ok {
		t.Fatal("finishTurnMode did not return visible error entry")
	}
	if entry.Role != session.RoleSystem ||
		entry.Content != "Error: turn finished without assistant response" {
		t.Fatalf("entry = %#v, want empty-assistant system error", entry)
	}
	if model.Progress.Mode != stateError ||
		model.Progress.LastError != "turn finished without assistant response" ||
		model.Progress.Status != "" {
		t.Fatalf("progress = %#v, want terminal error", model.Progress)
	}
	if model.InFlight.Thinking ||
		model.InFlight.Pending != nil ||
		len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("in-flight = %#v, want active state cleared", model.InFlight)
	}
}

func TestTurnReducerCompleteToolResultPromotesNextTool(t *testing.T) {
	model := readyModel(t)
	toolA := &session.Entry{Role: session.RoleTool, Content: "a partial"}
	toolB := &session.Entry{Role: session.RoleTool, Content: "b partial"}
	model.Progress.Mode = stateWorking
	model.Progress.Status = "Running tools..."
	model.Progress.ContextTokens = 456
	model.InFlight.Pending = toolA
	model.InFlight.PendingTools = map[string]*session.Entry{
		"tool-a": toolA,
		"tool-b": toolB,
	}

	entry, ok := model.turnReducer().completeToolResult("tool-a", session.ToolResultEvent{
		ToolUseID: "tool-a",
		Result:    "a done",
	})
	if !ok {
		t.Fatal("completeToolResult did not return completed tool")
	}
	if entry.Content != "a done" {
		t.Fatalf("entry content = %q, want a done", entry.Content)
	}
	if _, ok := model.InFlight.PendingTools["tool-a"]; ok {
		t.Fatalf("tool-a still pending: %#v", model.InFlight.PendingTools)
	}
	if model.InFlight.Pending != toolB {
		t.Fatalf("pending = %#v, want tool-b promoted", model.InFlight.Pending)
	}
	if model.Progress.Mode != stateWorking ||
		model.Progress.Status != "Running tools..." ||
		model.Progress.ContextTokens != 456 {
		t.Fatalf("progress changed before final tool finished: %#v", model.Progress)
	}

	entry, ok = model.turnReducer().completeToolResult("tool-b", session.ToolResultEvent{
		ToolUseID: "tool-b",
		Result:    "b done",
	})
	if !ok {
		t.Fatal("completeToolResult did not return final completed tool")
	}
	if entry.Content != "b done" {
		t.Fatalf("entry content = %q, want b done", entry.Content)
	}
	if model.InFlight.Pending != nil ||
		len(model.InFlight.PendingTools) != 0 ||
		model.Progress.Mode != stateIonizing ||
		model.Progress.Status != "" ||
		model.Progress.ContextTokens != 0 {
		t.Fatalf(
			"final tool did not clear active tool state: in-flight=%#v progress=%#v",
			model.InFlight,
			model.Progress,
		)
	}
}

func TestTurnReducerChildLifecycleSettlesProgress(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true

	child := model.turnReducer().requestChild("worker", "inspect")
	if child.Name != "worker" ||
		child.Intent != "inspect" ||
		model.Progress.Mode != stateWorking {
		t.Fatalf("requested child = %#v progress=%#v", child, model.Progress)
	}

	if !model.turnReducer().startChild("worker") {
		t.Fatal("startChild returned false")
	}
	if !model.turnReducer().appendChildDelta("worker", "partial") {
		t.Fatal("appendChildDelta returned false")
	}
	if got := model.InFlight.Subagents["worker"].Output; got != "partial" {
		t.Fatalf("child output = %q, want partial", got)
	}

	entry, ok := model.turnReducer().completeChild("worker", "done", time.Time{})
	if !ok {
		t.Fatal("completeChild returned false")
	}
	if entry.Role != session.RoleSubagent ||
		entry.Title != "worker" ||
		entry.Content != "Completed: done" {
		t.Fatalf("completion entry = %#v", entry)
	}
	if len(model.InFlight.Subagents) != 0 ||
		model.Progress.Status != "" ||
		model.Progress.Mode != stateIonizing {
		t.Fatalf("settled state = inFlight=%#v progress=%#v", model.InFlight, model.Progress)
	}
}

func TestTurnReducerChildFailureOwnsErrorState(t *testing.T) {
	model := readyModel(t)
	model.turnReducer().requestChild("worker", "inspect")

	entry, ok := model.turnReducer().failChild("worker", "boom", time.Time{})
	if !ok {
		t.Fatal("failChild returned false")
	}
	if entry.Role != session.RoleSubagent ||
		!entry.IsError ||
		entry.Content != "Failed: boom" {
		t.Fatalf("failure entry = %#v", entry)
	}
	if len(model.InFlight.Subagents) != 0 ||
		model.Progress.Mode != stateError ||
		model.Progress.LastError != "Subagent failed: boom" {
		t.Fatalf("failure state = inFlight=%#v progress=%#v", model.InFlight, model.Progress)
	}
}
