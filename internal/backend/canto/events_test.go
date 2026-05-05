package canto

import (
	"context"
	"testing"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func TestTranslateEventsCommitsAssistantFromMessageAdded(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewEvent("session-id", csession.MessageAdded, llm.Message{
		Role:      llm.RoleAssistant,
		Content:   "done",
		Reasoning: "brief reasoning",
	})
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	committed, ok := ev1.(ionsession.AgentMessage)
	if !ok {
		t.Fatalf("first event = %T, want AgentMessage", ev1)
	}
	if committed.Message != "done" || committed.Reasoning != "brief reasoning" {
		t.Fatalf("committed message = %#v", committed)
	}

	ev2 := receiveEvent(t, b.Events())
	if _, ok := ev2.(ionsession.TurnFinished); !ok {
		t.Fatalf("second event = %T, want TurnFinished", ev2)
	}

	ev3 := receiveEvent(t, b.Events())
	status, ok := ev3.(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("third event = %T, want StatusChanged", ev3)
	}
	if status.Status != "Ready" {
		t.Fatalf("status = %q, want Ready", status.Status)
	}
}

func TestTranslateEventsTurnCompletedDoesNotEmitEmptyAssistant(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	if _, ok := ev1.(ionsession.TurnFinished); !ok {
		t.Fatalf("first event = %T, want TurnFinished", ev1)
	}

	ev2 := receiveEvent(t, b.Events())
	status, ok := ev2.(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("second event = %T, want StatusChanged", ev2)
	}
	if status.Status != "Ready" {
		t.Fatalf("status = %q, want Ready", status.Status)
	}
}

func TestTranslateEventsClearsActiveTurnBeforeFinishedEvent(t *testing.T) {
	b := New()
	b.turnSeq = 7
	b.turnActive = true
	b.cancel = func() {}

	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	b.translateEvents(t.Context(), events, 7)

	if b.turnActive {
		t.Fatal("turnActive remained true after terminal event translation")
	}
	if b.cancel != nil {
		t.Fatal("cancel func remained set after terminal event translation")
	}
	ev := receiveEvent(t, b.Events())
	if _, ok := ev.(ionsession.TurnFinished); !ok {
		t.Fatalf("event = %T, want TurnFinished", ev)
	}
}

func TestTranslateEventsSuppressesCanceledTerminalError(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
		Error: context.Canceled.Error(),
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	if _, ok := ev1.(ionsession.TurnFinished); !ok {
		t.Fatalf("first event = %T, want TurnFinished", ev1)
	}

	ev2 := receiveEvent(t, b.Events())
	status, ok := ev2.(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("second event = %T, want StatusChanged", ev2)
	}
	if status.Status != "Ready" {
		t.Fatalf("status = %q, want Ready", status.Status)
	}
}

func TestTranslateEventsPreservesToolUseID(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewToolStartedEvent("session-id", csession.ToolStartedData{
		ID:        "tool-call-1",
		Tool:      "bash",
		Arguments: "git status",
	})
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "ok",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	started, ok := ev1.(ionsession.ToolCallStarted)
	if !ok {
		t.Fatalf("first event = %T, want ToolCallStarted", ev1)
	}
	if started.ToolUseID != "tool-call-1" {
		t.Fatalf("started id = %q, want tool-call-1", started.ToolUseID)
	}
	_ = receiveEvent(t, b.Events()) // status

	ev3 := receiveEvent(t, b.Events())
	result, ok := ev3.(ionsession.ToolResult)
	if !ok {
		t.Fatalf("third event = %T, want ToolResult", ev3)
	}
	if result.ToolUseID != "tool-call-1" {
		t.Fatalf("result id = %q, want tool-call-1", result.ToolUseID)
	}
}

func TestTranslateEventsPreservesToolOutputDeltaID(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewEvent("session-id", csession.ToolOutputDelta, map[string]string{
		"id":    "tool-call-1",
		"tool":  "bash",
		"delta": "partial output",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev := receiveEvent(t, b.Events())
	delta, ok := ev.(ionsession.ToolOutputDelta)
	if !ok {
		t.Fatalf("event = %T, want ToolOutputDelta", ev)
	}
	if delta.ToolUseID != "tool-call-1" {
		t.Fatalf("delta id = %q, want tool-call-1", delta.ToolUseID)
	}
	if delta.Delta != "partial output" {
		t.Fatalf("delta = %q, want partial output", delta.Delta)
	}
}

func TestTranslateEventsPreservesToolCompletedError(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "partial output\nError: exit status 1",
		Error:  "exit status 1",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev := receiveEvent(t, b.Events())
	result, ok := ev.(ionsession.ToolResult)
	if !ok {
		t.Fatalf("event = %T, want ToolResult", ev)
	}
	if result.Error == nil || result.Error.Error() != "exit status 1" {
		t.Fatalf("tool result error = %v, want exit status 1", result.Error)
	}
	if result.Result != "partial output\nError: exit status 1" {
		t.Fatalf("tool result output = %q", result.Result)
	}
}

func TestTranslateEventsUsesChildIDForSubagentRows(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewChildRequestedEvent("session-id", csession.ChildRequestedData{
		ChildID:        "explorer-123",
		ChildSessionID: "child-session",
		Task:           "inspect policy flow",
		AgentID:        "explorer",
		Mode:           csession.ChildModeHandoff,
	})
	events <- csession.NewChildStartedEvent("session-id", csession.ChildStartedData{
		ChildID:        "explorer-123",
		ChildSessionID: "child-session",
		AgentID:        "explorer",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	requested, ok := receiveEvent(t, b.Events()).(ionsession.ChildRequested)
	if !ok {
		t.Fatal("first event is not ChildRequested")
	}
	if requested.AgentName != "explorer-123" {
		t.Fatalf("requested agent name = %q, want child id", requested.AgentName)
	}
	_ = receiveEvent(t, b.Events()) // request status

	started, ok := receiveEvent(t, b.Events()).(ionsession.ChildStarted)
	if !ok {
		t.Fatal("third event is not ChildStarted")
	}
	if started.AgentName != "explorer-123" {
		t.Fatalf("started agent name = %q, want child id", started.AgentName)
	}
}
