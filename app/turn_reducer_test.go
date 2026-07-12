package app

import (
	"testing"
	"time"

	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
)

func TestTurnReducerClearActiveStateCanKeepQueuedTurns(t *testing.T) {
	model := readyModel(t)
	tool := testToolEntry("tool", "partial", false)
	model.InFlight.Thinking = true
	var toolEntry session.Entry = tool
	model.InFlight.Pending = &toolEntry
	model.InFlight.PendingTools = map[string]session.Entry{"tool-a": tool}
	model.InFlight.Subagents = map[string]*SubagentProgress{"child": {ID: "child"}}
	model.InFlight.QueuedTurns = []string{"follow up"}
	model.InFlight.StreamBuf = "stream"
	model.InFlight.ReasonBuf = "reason"
	model.InFlight.AgentCommitted = true
	model.InFlight.DrainUntilTurnStarted = true
	model.InFlight.DrainStartedAt = time.Now()
	model.Progress.LastToolUseID = "tool-a"
	model.Progress.ContextTokens = 123

	model.turnReducer().ClearActiveState(false)

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

	model.turnReducer().ClearActiveState(true)
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want cleared", model.InFlight.QueuedTurns)
	}
}

func TestTurnReducerFinishesPendingAssistantFromStream(t *testing.T) {
	model := readyModel(t)
	pending := &session.MessageEntry{
		Message: &session.AssistantMessage{},
	}
	var pendEntry session.Entry = pending
	model.InFlight.Pending = &pendEntry
	model.InFlight.StreamBuf = "answer"
	model.InFlight.ReasonBuf = "reasoning"
	model.InFlight.AgentCommitted = true

	entry, completed, ok := model.turnReducer().FinishPendingAssistant()
	if !ok {
		t.Fatal("finishPendingAssistant did not return pending stream entry")
	}
	if !completed {
		t.Fatal("assistantCompleted = false, want true")
	}
	if session.EntryText(entry) != "answer" || session.EntryReasoning(entry) != "reasoning" {
		t.Fatalf("entry = text %q reasoning %q, want streamed answer with reasoning",
			session.EntryText(entry), session.EntryReasoning(entry))
	}
	if model.InFlight.Pending != nil ||
		model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" {
		t.Fatalf("pending stream state not cleared: %#v", model.InFlight)
	}
}

func TestTurnReducerFinishModeClearsStaleStateOnEmptyAssistant(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = runtime.StateWorking
	model.Progress.Status = "Running bash..."
	model.Progress.LastError = ""
	model.InFlight.Thinking = true
	model.InFlight.QueuedTurns = []string{"stale follow-up"}
	pending := testAgentEntry("", "")
	var pendEntry session.Entry = pending
	model.InFlight.Pending = &pendEntry

	entry, ok := model.turnReducer().FinishTurnMode(false)
	if !ok {
		t.Fatal("finishTurnMode did not return visible error entry")
	}
	if session.EntryRole(entry) != session.RoleUser ||
		session.EntryText(entry) != "Error: turn finished without assistant response" {
		t.Fatalf("entry = role %q text %q, want system error",
			session.EntryRole(entry), session.EntryText(entry))
	}
	if model.Progress.Mode != runtime.StateError ||
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
	toolA := testToolEntry("a", "a partial", false)
	toolB := testToolEntry("b", "b partial", false)
	model.Progress.Mode = runtime.StateWorking
	model.Progress.Status = "Running tools..."
	model.Progress.ContextTokens = 456
	var toolAEntry session.Entry = toolA
	model.InFlight.Pending = &toolAEntry
	model.InFlight.PendingTools = map[string]session.Entry{
		"tool-a": toolA,
		"tool-b": toolB,
	}

	entry, ok := model.turnReducer().CompleteToolResult("tool-a", session.ToolExecEnd{
		ToolCallID: "tool-a",
		Result: session.ToolResultMessage{
			ToolCallID: "tool-a",
			Content:    []session.Content{session.TextContent{Text: "a done"}},
		},
	})
	if !ok {
		t.Fatal("completeToolResult did not return completed tool")
	}
	if session.EntryText(entry) != "a done" {
		t.Fatalf("entry content = %q, want a done", session.EntryText(entry))
	}
	if _, ok := model.InFlight.PendingTools["tool-a"]; ok {
		t.Fatalf("tool-a still pending: %#v", model.InFlight.PendingTools)
	}
	// Pending should be promoted to toolB
	if model.InFlight.Pending == nil || session.EntryText(*model.InFlight.Pending) != "b partial" {
		t.Fatalf("pending = %#v, want tool-b promoted", model.InFlight.Pending)
	}
	if model.Progress.Mode != runtime.StateWorking ||
		model.Progress.Status != "Running tools..." ||
		model.Progress.ContextTokens != 456 {
		t.Fatalf("progress changed before final tool finished: %#v", model.Progress)
	}

	entry, ok = model.turnReducer().CompleteToolResult("tool-b", session.ToolExecEnd{
		ToolCallID: "tool-b",
		Result: session.ToolResultMessage{
			ToolCallID: "tool-b",
			Content:    []session.Content{session.TextContent{Text: "b done"}},
		},
	})
	if !ok {
		t.Fatal("completeToolResult did not return final completed tool")
	}
	if session.EntryText(entry) != "b done" {
		t.Fatalf("entry content = %q, want b done", session.EntryText(entry))
	}
	if model.InFlight.Pending != nil ||
		len(model.InFlight.PendingTools) != 0 ||
		model.Progress.Mode != runtime.StateIonizing ||
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

	child := model.turnReducer().RequestChild("worker", "inspect")
	if child.Name != "worker" ||
		child.Intent != "inspect" ||
		model.Progress.Mode != runtime.StateWorking {
		t.Fatalf("requested child = %#v progress=%#v", child, model.Progress)
	}

	if !model.turnReducer().StartChild("worker") {
		t.Fatal("startChild returned false")
	}
	if !model.turnReducer().AppendChildDelta("worker", "partial") {
		t.Fatal("appendChildDelta returned false")
	}
	if got := model.InFlight.Subagents["worker"].Output; got != "partial" {
		t.Fatalf("child output = %q, want partial", got)
	}

	entry, ok := model.turnReducer().CompleteChild("worker", "done", time.Time{})
	if !ok {
		t.Fatal("completeChild returned false")
	}
	if session.EntryText(entry) != "Completed: done" {
		t.Fatalf("completion entry text = %q, want Completed: done", session.EntryText(entry))
	}
	if len(model.InFlight.Subagents) != 0 ||
		model.Progress.Status != "" ||
		model.Progress.Mode != runtime.StateIonizing {
		t.Fatalf("settled state = inFlight=%#v progress=%#v", model.InFlight, model.Progress)
	}
}

func TestTurnReducerChildFailureOwnsErrorState(t *testing.T) {
	model := readyModel(t)
	model.turnReducer().RequestChild("worker", "inspect")

	entry, ok := model.turnReducer().FailChild("worker", "boom", time.Time{})
	if !ok {
		t.Fatal("failChild returned false")
	}
	if session.EntryText(entry) != "Failed: boom" {
		t.Fatalf("failure entry text = %q, want Failed: boom", session.EntryText(entry))
	}
	if len(model.InFlight.Subagents) != 0 ||
		model.Progress.Mode != runtime.StateError ||
		model.Progress.LastError != "Subagent failed: boom" {
		t.Fatalf("failure state = inFlight=%#v progress=%#v", model.InFlight, model.Progress)
	}
}
