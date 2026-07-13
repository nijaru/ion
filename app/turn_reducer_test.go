package app

import (
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
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
	model.Progress.Mode = StateWorking
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
	if model.Progress.Mode != StateError ||
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
	model.Progress.Mode = StateWorking
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
	if model.Progress.Mode != StateWorking ||
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
		model.Progress.Mode != StateIonizing ||
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
		model.Progress.Mode != StateWorking {
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
		model.Progress.Mode != StateIonizing {
		t.Fatalf("settled state = inFlight=%#v progress=%#v", model.InFlight, model.Progress)
	}
}

func TestSessionAssistantStreamUpdatesPlaneB(t *testing.T) {
	model := readyModel(t)
	now := time.Now()
	model, _ = model.handleSessionEvent(session.TurnStart{Timestamp: now})
	model, _ = model.handleSessionEvent(session.MessageStart{Message: &session.AssistantMessage{Timestamp: now}})
	model, _ = model.handleSessionEvent(session.MessageUpdate{
		Message: &session.AssistantMessage{
			Content:   []session.Content{session.TextContent{Text: "hello from stream"}},
			Timestamp: now,
		},
		Delta:     session.TextDelta{Text: "hello from stream"},
		Timestamp: now,
	})
	got := ansi.Strip(model.renderPlaneB())
	if !strings.Contains(got, "hello from stream") {
		t.Fatalf("plane B = %q, want streamed text", got)
	}
	if strings.Contains(got, "{Text:") {
		t.Fatalf("plane B rendered a Go delta struct: %q", got)
	}
}

func TestTurnReducerStartAndUpdateAssistantMessageKeepsTypedContent(t *testing.T) {
	model := readyModel(t)
	started := &session.AssistantMessage{Content: []session.Content{session.TextContent{Text: "hello"}}}
	model.turnReducer().StartAssistantMessage(started)
	if got := model.turnReducer().AgentStreamContent(); got != "hello" {
		t.Fatalf("initial stream = %q, want hello", got)
	}
	updated := &session.AssistantMessage{Content: []session.Content{
		session.ThinkingContent{Text: "think"},
		session.TextContent{Text: "hello world"},
	}}
	model.turnReducer().UpdateAssistantMessage(updated)
	if got := model.turnReducer().AgentStreamContent(); got != "hello world" {
		t.Fatalf("updated stream = %q, want hello world", got)
	}
	if got := model.InFlight.ReasonBuf; got != "think" {
		t.Fatalf("reasoning = %q, want think", got)
	}
	if model.InFlight.Pending == nil || session.EntryText(*model.InFlight.Pending) != "hello world" {
		t.Fatalf("pending = %#v, want updated assistant", model.InFlight.Pending)
	}
}

func TestTurnReducerTypedDeltaFallbackDoesNotStringifyStructs(t *testing.T) {
	model := readyModel(t)
	model.turnReducer().AppendAgentDelta("", session.TextDelta{Text: "hello"}, time.Now())
	model.turnReducer().AppendThinkingDelta("", session.ThinkingDelta{Text: "think"})
	if got := model.InFlight.StreamBuf; got != "hello" {
		t.Fatalf("stream = %q, want hello", got)
	}
	if got := model.InFlight.ReasonBuf; got != "think" {
		t.Fatalf("reasoning = %q, want think", got)
	}
	if model.InFlight.Pending == nil || session.EntryText(*model.InFlight.Pending) != "hello" {
		t.Fatalf("fallback pending = %#v, want hello assistant", model.InFlight.Pending)
	}
}

func TestTurnReducerStartTurnAndDispatchManageBusyLifecycle(t *testing.T) {
	model := readyModel(t)
	now := time.Now()
	model.turnReducer().StartTurn(now, now)
	if !model.InFlight.Thinking || model.Progress.Mode != StateStreaming {
		t.Fatalf("turn state = inFlight=%#v progress=%#v, want active streaming", model.InFlight, model.Progress)
	}
	model.turnReducer().QueueTurn("follow up")
	dispatch := model.turnReducer().FinishTurnDispatch()
	if dispatch.Action != TurnFinishedDispatchSubmitLocal || dispatch.Text != "follow up" || !dispatch.RearmSessionEvents {
		t.Fatalf("dispatch = %#v, want local follow-up submit", dispatch)
	}
	if got := model.turnReducer().FinishTurnDispatch(); !got.AwaitNext {
		t.Fatalf("empty dispatch = %#v, want await", got)
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
		model.Progress.Mode != StateError ||
		model.Progress.LastError != "Subagent failed: boom" {
		t.Fatalf("failure state = inFlight=%#v progress=%#v", model.InFlight, model.Progress)
	}
}
