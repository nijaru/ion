package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestModelStreamsAndCommitsPendingEntry(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.AgentDelta{Delta: "streamed reply"})
	model = updated.(Model)

	if model.InFlight.Pending == nil || model.InFlight.Pending.Content != "streamed reply" {
		t.Fatalf("expected pending streamed agent entry, got %#v", model.InFlight.Pending)
	}

	updated, cmd := model.Update(session.AgentMessage{})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("expected pending entry to be cleared after flush")
	}

	// Verify that a Println command was returned
	if cmd == nil {
		t.Fatalf("expected tea.Println command after finalizing message")
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.Agent); ok {
			t.Fatalf("agent message should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestUserMessagePrintsFromSessionEvent(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(session.UserMessage{Message: "read README.md"})
	model = updated.(Model)

	if cmd == nil {
		t.Fatal("expected committed user print command")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("user message event did not mark transcript printed")
	}
	if model.InFlight.Pending != nil || model.InFlight.StreamBuf != "" {
		t.Fatalf("user event changed in-flight assistant state: %#v", model.InFlight)
	}
}

func TestPlaneBShowsPendingAgentText(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 24
	model.InFlight.Pending = &session.Entry{
		Role:    session.Agent,
		Content: "streamed reply with a long tail",
	}

	got := ansi.Strip(model.renderPlaneB())
	if !strings.Contains(got, "• streamed reply with") ||
		!strings.Contains(got, "\n  long tail") {
		t.Fatalf("plane B = %q, want wrapped live assistant text", got)
	}
}

func TestPlaneBShowsPendingAgentTextWithoutMarkdownRendering(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 80
	model.InFlight.Pending = &session.Entry{
		Role: session.Agent,
		Content: strings.Join([]string{
			"Working:",
			"",
			"```go",
			"fmt.Println(\"streaming\")",
		}, "\n"),
	}

	got := ansi.Strip(model.renderPlaneB())
	for _, want := range []string{
		"• Working:",
		"  ```go",
		"  fmt.Println(\"streaming\")",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("plane B = %q, want raw live markdown fragment %q", got, want)
		}
	}
}

func TestPlaneBTrimsLeadingNewlinesFromPendingAgentText(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 80
	model.InFlight.Pending = &session.Entry{
		Role:    session.Agent,
		Content: "\n\n- first streamed bullet",
	}

	got := ansi.Strip(model.renderPlaneB())
	if strings.HasPrefix(got, "•\n") || strings.HasPrefix(got, "• \n") {
		t.Fatalf("plane B = %q, want no empty bullet row", got)
	}
	if !strings.Contains(got, "• - first streamed bullet") {
		t.Fatalf("plane B = %q, want leading markdown text on first row", got)
	}
}

func TestLateAgentDeltaAfterCommitIsIgnored(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.AgentDelta{Delta: "partial"})
	model = updated.(Model)
	updated, cmd := model.Update(session.AgentMessage{Message: "final"})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected committed assistant print command")
	}
	if !model.InFlight.AgentCommitted {
		t.Fatal("agent commit marker was not set")
	}

	updated, _ = model.Update(session.AgentDelta{Delta: "late"})
	model = updated.(Model)
	if model.InFlight.Pending != nil || model.InFlight.StreamBuf != "" {
		t.Fatalf(
			"late delta recreated pending stream: pending=%#v stream=%q",
			model.InFlight.Pending,
			model.InFlight.StreamBuf,
		)
	}

	updated, _ = model.Update(session.ThinkingDelta{Delta: "late thinking"})
	model = updated.(Model)
	if model.InFlight.ReasonBuf != "" {
		t.Fatalf("late thinking buffer = %q, want ignored", model.InFlight.ReasonBuf)
	}

	updated, _ = model.Update(session.TurnFinished{})
	model = updated.(Model)
	if model.InFlight.Pending != nil || model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" {
		t.Fatalf(
			"turn finish left pending stream: pending=%#v stream=%q reason=%q",
			model.InFlight.Pending,
			model.InFlight.StreamBuf,
			model.InFlight.ReasonBuf,
		)
	}
}

func TestToolEntryFlushesToTranscript(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess
	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Args:      "ls",
	})
	model = updated.(Model)

	if model.InFlight.Pending == nil || model.InFlight.Pending.Role != session.Tool {
		t.Fatalf("expected pending tool entry")
	}
	model.Progress.Status = "Running bash..."

	updated, cmd := model.Update(session.ToolResult{
		ToolName: "bash",
		Result:   "ok",
	})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("expected pending entry to be cleared")
	}
	if cmd == nil {
		t.Fatalf("expected tea.Println command for tool result")
	}
	if model.Progress.Mode != stateIonizing {
		t.Fatalf("progress mode = %v, want ionizing after tool completion", model.Progress.Mode)
	}
	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared after tool completion", model.Progress.Status)
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.ToolResult); ok {
			t.Fatalf("tool result should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestTokenUsageSeparatesSessionTotalsFromContextEstimate(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.TokenUsage{Input: 100, Output: 10, Cost: 0.01})
	model = updated.(Model)
	updated, _ = model.Update(session.TokenUsage{Input: 20, Output: 5, Cost: 0.02})
	model = updated.(Model)

	if model.Progress.TokensSent != 120 || model.Progress.TokensReceived != 15 {
		t.Fatalf(
			"session totals = %d/%d, want 120/15",
			model.Progress.TokensSent,
			model.Progress.TokensReceived,
		)
	}
	if model.Progress.ContextTokens != 135 {
		t.Fatalf("context tokens = %d, want 135", model.Progress.ContextTokens)
	}
	if model.Progress.TotalCost != 0.03 || model.Progress.CurrentTurnCost != 0.03 {
		t.Fatalf(
			"cost totals = %.3f/%.3f, want 0.030/0.030",
			model.Progress.TotalCost,
			model.Progress.CurrentTurnCost,
		)
	}
}

func TestTokenUsagePersistenceReturnsBeforeStorageAppendCompletes(t *testing.T) {
	storageSess := &blockingAppendStorage{
		entered: make(chan any, 1),
		release: make(chan struct{}),
	}
	model := readyModel(t)
	model.Model.Storage = storageSess

	next, cmd := model.Update(session.TokenUsage{Input: 10, Output: 2, Cost: 0.01})
	model = next.(Model)

	if model.Progress.TokensSent != 10 || model.Progress.TokensReceived != 2 {
		t.Fatalf(
			"token totals = %d/%d, want 10/2",
			model.Progress.TokensSent,
			model.Progress.TokensReceived,
		)
	}
	select {
	case event := <-storageSess.entered:
		t.Fatalf("append ran during Update: %#v", event)
	default:
	}

	done := make(chan []tea.Msg, 1)
	go func() {
		done <- runSequencePrefix(t, cmd, 1)
	}()
	select {
	case event := <-storageSess.entered:
		usage, ok := event.(storage.TokenUsage)
		if !ok {
			t.Fatalf("append event = %#v, want token usage", event)
		}
		if usage.Input != 10 || usage.Output != 2 || usage.Cost != 0.01 {
			t.Fatalf("token usage = %#v, want 10/2/0.01", usage)
		}
	case <-time.After(time.Second):
		t.Fatal("token persistence command did not start append")
	}
	close(storageSess.release)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("token persistence command did not finish")
	}
	if len(storageSess.appends) != 1 {
		t.Fatalf("appends after command = %#v, want one token usage append", storageSess.appends)
	}
}

func TestToolResultStartsFreshContextEstimateForNextProviderCall(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.TokenUsage{Input: 100, Output: 10})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolCallStarted{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Args:      "echo ok",
	})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Result:    "ok\n",
	})
	model = updated.(Model)

	if model.Progress.ContextTokens != 0 {
		t.Fatalf("context tokens after tool = %d, want reset", model.Progress.ContextTokens)
	}

	updated, _ = model.Update(session.TokenUsage{Input: 140, Output: 20})
	model = updated.(Model)

	if model.Progress.TokensSent != 240 || model.Progress.TokensReceived != 30 {
		t.Fatalf(
			"session totals = %d/%d, want 240/30",
			model.Progress.TokensSent,
			model.Progress.TokensReceived,
		)
	}
	if model.Progress.ContextTokens != 160 {
		t.Fatalf("context tokens = %d, want 160", model.Progress.ContextTokens)
	}
}

func TestAgentDeltaDoesNotAppendToPendingToolEntry(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolCallStarted{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Args:      "echo ok",
	})
	model = updated.(Model)
	updated, _ = model.Update(session.AgentDelta{Delta: "assistant text"})
	model = updated.(Model)

	if model.InFlight.Pending == nil || model.InFlight.Pending.Role != session.Agent {
		t.Fatalf("pending entry = %#v, want agent entry", model.InFlight.Pending)
	}
	if model.InFlight.Pending.Content != "assistant text" {
		t.Fatalf("pending agent content = %q, want assistant text", model.InFlight.Pending.Content)
	}
	tool := model.InFlight.PendingTools["tool-call-1"]
	if tool == nil {
		t.Fatal("pending tool entry was lost")
	}
	if tool.Content != "" {
		t.Fatalf("pending tool content = %q, want unchanged", tool.Content)
	}
}

func TestAgentMessagePrintsWithoutPendingStream(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, cmd := model.Update(session.AgentMessage{Message: "done"})
	model = updated.(Model)

	if cmd == nil {
		t.Fatal("expected print command for committed assistant message")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("committed assistant message did not mark transcript printed")
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.Agent); ok {
			t.Fatalf("agent message should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestAgentMessageAfterToolResultPrintsFinalAnswer(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolCallStarted{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Args:      "echo ok",
	})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Result:    "ok\n",
	})
	model = updated.(Model)

	model.App.PrintedTranscript = false
	updated, cmd := model.Update(session.AgentMessage{Message: "done"})
	model = updated.(Model)

	if cmd == nil {
		t.Fatal("expected print command for final assistant message after tool result")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("final assistant message after tool result did not mark transcript printed")
	}
	if model.InFlight.Pending != nil {
		t.Fatalf("pending entry = %#v, want none", model.InFlight.Pending)
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.Agent); ok {
			t.Fatalf("agent message should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestInterleavedToolResultsPreservePendingEntries(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-a",
		ToolName:  "bash",
		Args:      "first",
	})
	model = updated.(Model)

	updated, _ = model.Update(session.ToolCallStarted{
		ToolUseID: "tool-b",
		ToolName:  "bash",
		Args:      "second",
	})
	model = updated.(Model)

	updated, _ = model.Update(session.ToolOutputDelta{ToolUseID: "tool-a", Delta: "a partial"})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolOutputDelta{ToolUseID: "tool-b", Delta: "b partial"})
	model = updated.(Model)

	if got := model.InFlight.PendingTools["tool-a"].Content; got != "a partial" {
		t.Fatalf("tool-a pending content = %q, want a partial", got)
	}
	if got := model.InFlight.PendingTools["tool-b"].Content; got != "b partial" {
		t.Fatalf("tool-b pending content = %q, want b partial", got)
	}

	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "tool-a",
		ToolName:  "bash",
		Result:    "a done",
	})
	model = updated.(Model)

	if _, ok := model.InFlight.PendingTools["tool-a"]; ok {
		t.Fatal("tool-a pending entry still present after result")
	}
	if got := model.InFlight.PendingTools["tool-b"].Content; got != "b partial" {
		t.Fatalf("tool-b pending content = %q, want b partial", got)
	}

	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "tool-b",
		ToolName:  "bash",
		Result:    "b done",
	})
	model = updated.(Model)

	if len(model.InFlight.PendingTools) != 0 {
		t.Fatalf("pending tools = %#v, want none", model.InFlight.PendingTools)
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.ToolResult); ok {
			t.Fatalf("tool results should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestUnknownToolResultIDDoesNotClearAnotherPendingTool(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-a",
		ToolName:  "bash",
		Args:      "first",
	})
	model = updated.(Model)

	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "missing-tool",
		ToolName:  "bash",
		Result:    "wrong result",
	})
	model = updated.(Model)

	if _, ok := model.InFlight.PendingTools["tool-a"]; !ok {
		t.Fatal("known pending tool was cleared by unknown tool result")
	}
	for _, event := range storageSess.appends {
		if result, ok := event.(storage.ToolResult); ok && result.ToolUseID == "missing-tool" {
			t.Fatal("unknown tool result was persisted")
		}
	}
}

func TestTurnFinishedLeavesProgressComplete(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.InFlight.Thinking = true
	model.InFlight.AgentCommitted = true
	model.Progress.TurnStartedAt = time.Now().Add(-3 * time.Second)
	model.Progress.CurrentTurnInput = 1200
	model.Progress.CurrentTurnOutput = 300

	updated, _ := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress = %v, want stateComplete", model.Progress.Mode)
	}
	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "✓ Complete") {
		t.Fatalf("progress line = %q, want complete state", line)
	}
	for _, want := range []string{"↑ 1.2k", "↓ 300", "3s"} {
		if !strings.Contains(line, want) {
			t.Fatalf("progress line = %q, missing %q", line, want)
		}
	}
	if strings.Index(line, "3s") < strings.Index(line, "↓ 300") {
		t.Fatalf("progress line = %q, want elapsed time after token counters", line)
	}
}

func TestTurnFinishedCommitsPendingStreamWhenNoAgentMessageArrives(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.InFlight.Pending = &session.Entry{Role: session.Agent, Content: "streamed answer"}
	model.InFlight.StreamBuf = "streamed answer"
	model.InFlight.ReasonBuf = "brief reasoning"
	model.InFlight.Thinking = true

	updated, cmd := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("pending agent entry = %#v, want flushed", model.InFlight.Pending)
	}
	if model.InFlight.StreamBuf != "" || model.InFlight.ReasonBuf != "" {
		t.Fatalf(
			"stream buffers = %q/%q, want cleared",
			model.InFlight.StreamBuf,
			model.InFlight.ReasonBuf,
		)
	}
	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress = %v, want complete", model.Progress.Mode)
	}
	if cmd == nil {
		t.Fatal("expected print command for flushed pending stream")
	}
}

func TestTurnFinishedWithoutAssistantResponseShowsError(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateWorking
	model.Progress.TurnStartedAt = time.Now().Add(-2 * time.Second)
	model.InFlight.Pending = &session.Entry{Role: session.Agent}
	model.InFlight.QueuedTurns = []string{"follow-up"}
	model.InFlight.Thinking = true

	updated, cmd := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "turn finished without assistant response" {
		t.Fatalf("last error = %q", model.Progress.LastError)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want cleared", model.InFlight.QueuedTurns)
	}
	if cmd == nil {
		t.Fatal("expected command to print visible error")
	}
}

func TestTurnFinishedWithoutAssistantResponseClearsPendingToolState(t *testing.T) {
	model := readyModel(t)
	toolEntry := &session.Entry{Role: session.Tool, Title: "Bash(sleep 10)"}
	model.Progress.Mode = stateWorking
	model.Progress.Status = "Running bash..."
	model.Progress.LastToolUseID = "tool-a"
	model.InFlight.Thinking = true
	model.InFlight.Pending = toolEntry
	model.InFlight.PendingTools = map[string]*session.Entry{"tool-a": toolEntry}

	updated, _ := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "turn finished without assistant response" {
		t.Fatalf("last error = %q", model.Progress.LastError)
	}
	if model.InFlight.Pending != nil ||
		len(model.InFlight.PendingTools) != 0 ||
		model.Progress.LastToolUseID != "" ||
		model.Progress.Status != "" {
		t.Fatalf(
			"terminal error left stale tool state: pending=%#v tools=%#v lastTool=%q status=%q",
			model.InFlight.Pending,
			model.InFlight.PendingTools,
			model.Progress.LastToolUseID,
			model.Progress.Status,
		)
	}
}

func TestChildLifecycleUpdatesPlaneB(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.handleSessionEvent(session.ChildRequested{
		AgentName: "worker-1",
		Query:     "inspect the repo",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] == nil ||
		model.InFlight.Subagents["worker-1"].Name != "worker-1" {
		t.Fatalf(
			"pending child after request = %#v, want subagent progress in Subagents map",
			model.InFlight.Subagents["worker-1"],
		)
	}
	if model.InFlight.Subagents["worker-1"].Name != "worker-1" {
		t.Fatalf("child name = %q, want worker-1", model.InFlight.Subagents["worker-1"].Name)
	}
	if model.InFlight.Subagents["worker-1"].Intent != "inspect the repo" {
		t.Fatalf("child intent = %q, want query", model.InFlight.Subagents["worker-1"].Intent)
	}

	updated, _ = model.handleSessionEvent(session.ChildStarted{
		AgentName: "worker-1",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] == nil ||
		model.InFlight.Subagents["worker-1"].Status != "Started" {
		t.Fatalf(
			"child status after start = %q, want Started",
			model.InFlight.Subagents["worker-1"].Status,
		)
	}

	updated, _ = model.handleSessionEvent(session.ChildDelta{
		AgentName: "worker-1",
		Delta:     "thinking...\n",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] == nil ||
		!strings.Contains(model.InFlight.Subagents["worker-1"].Output, "thinking...") {
		t.Fatalf(
			"child output after delta = %#v, want streamed delta",
			model.InFlight.Subagents["worker-1"],
		)
	}

	updated, _ = model.handleSessionEvent(session.ChildCompleted{
		AgentName: "worker-1",
		Result:    "done",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] != nil {
		t.Fatalf("expected child entry to clear, got %#v", model.InFlight.Subagents["worker-1"])
	}
	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress mode after child complete = %v, want stateComplete", model.Progress.Mode)
	}

	updated, _ = model.handleSessionEvent(session.ChildRequested{
		AgentName: "worker-2",
		Query:     "recover from failure",
	})
	model = updated

	updated, _ = model.handleSessionEvent(session.ChildFailed{
		AgentName: "worker-2",
		Error:     "boom",
	})
	model = updated
	if model.InFlight.Subagents["worker-2"] != nil {
		t.Fatalf(
			"expected failed child entry to clear, got %#v",
			model.InFlight.Subagents["worker-2"],
		)
	}
	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode after child failure = %v, want stateError", model.Progress.Mode)
	}
	if model.Progress.LastError != "Subagent failed: boom" {
		t.Fatalf(
			"last error after child failure = %q, want subagent error",
			model.Progress.LastError,
		)
	}
}

func TestChildCompletedDuringTurnDoesNotMarkParentComplete(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.Progress.Mode = stateWorking
	model.Progress.Status = "Running subagent..."
	model.InFlight.Subagents["worker-1"] = &SubagentProgress{
		ID:     "worker-1",
		Name:   "worker-1",
		Intent: "inspect the repo",
		Status: "Started",
	}

	updated, _ := model.handleSessionEvent(session.ChildCompleted{
		AgentName: "worker-1",
		Result:    "done",
	})
	model = updated

	if model.Progress.Mode == stateComplete {
		t.Fatal("child completion marked parent turn complete")
	}
	if model.Progress.Mode != stateIonizing {
		t.Fatalf("progress mode = %v, want parent turn to keep running", model.Progress.Mode)
	}
	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared after child completion", model.Progress.Status)
	}
	if model.InFlight.Subagents["worker-1"] != nil {
		t.Fatalf("child entry = %#v, want cleared", model.InFlight.Subagents["worker-1"])
	}
}

func TestChildBlockedUpdatesPlaneB(t *testing.T) {
	model := readyModel(t)

	next, _ := model.Update(session.ChildRequested{
		AgentName: "worker-3",
		Query:     "wait for approval",
	})
	model = next.(Model)

	next, _ = model.Update(session.ChildBlocked{
		AgentName: "worker-3",
		Reason:    "needs approval",
	})
	model = next.(Model)

	if model.InFlight.Subagents["worker-3"] == nil ||
		model.InFlight.Subagents["worker-3"].Name != "worker-3" {
		t.Fatalf(
			"pending child after block = %#v, want subagent progress in Subagents map",
			model.InFlight.Subagents["worker-3"],
		)
	}
	if got := model.InFlight.Subagents["worker-3"].Output; !strings.Contains(
		got,
		"BLOCKED: needs approval",
	) {
		t.Fatalf("child output = %q, want blocked notice", got)
	}
	if model.Progress.Mode != stateBlocked {
		t.Fatalf("progress mode = %v, want stateBlocked", model.Progress.Mode)
	}
	if model.InFlight.Thinking {
		t.Fatal("blocked child should stop the active thinking spinner")
	}
	if got := ansi.Strip(model.progressLine()); !strings.Contains(got, "Subagent blocked") {
		t.Fatalf("progress line = %q, want blocked state", got)
	}
}

func TestSessionErrorClearsQueuedTurnsAndSetsError(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{"stale follow up"}
	model.Progress.LastError = "old error"

	next, _ := model.Update(session.Error{Err: errors.New("backend failed")})
	model = next.(Model)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want cleared on session error", model.InFlight.QueuedTurns)
	}
	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "backend failed" {
		t.Fatalf("last error = %q, want backend failed", model.Progress.LastError)
	}
}

func TestSessionErrorPersistenceFailureKeepsReducerArmed(t *testing.T) {
	storageSess := &stubStorageSession{appendErr: errors.New("disk full")}
	model := readyModel(t)
	model.Model.Storage = storageSess
	model.InFlight.QueuedTurns = []string{"stale follow up"}

	next, cmd := model.Update(session.Error{Err: errors.New("backend failed")})
	model = next.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "backend failed" {
		t.Fatalf("last error = %q, want backend failed", model.Progress.LastError)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want cleared", model.InFlight.QueuedTurns)
	}
	requireSequenceCmd(t, cmd)
	msgs := runSequencePrefix(t, cmd, 2)
	var foundPersistError bool
	for _, msg := range msgs {
		if local, ok := msg.(localErrorMsg); ok &&
			strings.Contains(local.err.Error(), "persist session error: disk full") {
			foundPersistError = true
		}
	}
	if !foundPersistError {
		t.Fatalf("command messages = %#v, want session persistence error", msgs)
	}
}

func TestSessionErrorPersistenceReturnsBeforeStorageAppendCompletes(t *testing.T) {
	storageSess := &blockingAppendStorage{
		entered: make(chan any, 1),
		release: make(chan struct{}),
	}
	model := readyModel(t)
	model.Model.Storage = storageSess

	next, cmd := model.Update(session.Error{Err: errors.New("backend failed")})
	model = next.(Model)

	if model.Progress.Mode != stateError || model.Progress.LastError != "backend failed" {
		t.Fatalf("progress = %v/%q, want backend failed error", model.Progress.Mode, model.Progress.LastError)
	}
	select {
	case event := <-storageSess.entered:
		t.Fatalf("append ran during Update: %#v", event)
	default:
	}

	done := make(chan []tea.Msg, 1)
	go func() {
		done <- runSequencePrefix(t, cmd, 2)
	}()
	select {
	case event := <-storageSess.entered:
		system, ok := event.(storage.System)
		if !ok {
			t.Fatalf("append event = %#v, want system", event)
		}
		if !strings.Contains(system.Content, "backend failed") {
			t.Fatalf("system content = %q, want backend failed", system.Content)
		}
	case <-time.After(time.Second):
		t.Fatal("session error persistence command did not start append")
	}
	close(storageSess.release)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("session error persistence command did not finish")
	}
	if len(storageSess.appends) != 1 {
		t.Fatalf("appends after command = %#v, want one system append", storageSess.appends)
	}
}

func TestSessionErrorWithoutErrUsesFallbackMessage(t *testing.T) {
	model := readyModel(t)

	next, _ := model.Update(session.Error{})
	model = next.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "session error" {
		t.Fatalf("last error = %q, want fallback", model.Progress.LastError)
	}
}

func TestSessionErrorSoftensEmptyAssistantResponse(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	next, cmd := model.Update(session.Error{
		Err: errors.New(
			"assistant response has no content, reasoning, thinking blocks, or tool calls",
		),
	})
	model = next.(Model)
	runSequencePrefix(t, cmd, 2)

	want := "Provider returned an empty response. Try again or switch models."
	if model.Progress.LastError != want {
		t.Fatalf("last error = %q, want %q", model.Progress.LastError, want)
	}
	if got := ansi.Strip(model.progressLine()); got != "" {
		t.Fatalf("progress line = %q, want printed transcript to own the error", got)
	}
	var sys storage.System
	for _, event := range storageSess.appends {
		if e, ok := event.(storage.System); ok {
			sys = e
		}
	}
	if !strings.Contains(sys.Content, want) {
		t.Fatalf("system error = %q, want friendly empty-response message", sys.Content)
	}
}

func TestStatusPersistenceFailureKeepsReducerArmed(t *testing.T) {
	storageSess := &stubStorageSession{appendErr: errors.New("disk full")}
	model := readyModel(t)
	model.Model.Storage = storageSess

	next, cmd := model.Update(session.StatusChanged{Status: "Thinking..."})
	model = next.(Model)

	if model.Progress.Status != "Thinking..." {
		t.Fatalf("status = %q, want Thinking...", model.Progress.Status)
	}
	requireSequenceCmd(t, cmd)
	msgs := runSequencePrefix(t, cmd, 1)
	if len(msgs) != 1 {
		t.Fatalf("persistence messages = %d, want 1", len(msgs))
	}
	if err := localErrorFromMsg(t, msgs[0]); !strings.Contains(err.Error(), "persist status: disk full") {
		t.Fatalf("persistence error = %v, want status append failure", err)
	}
}

type blockingAppendStorage struct {
	stubStorageSession
	entered chan any
	release chan struct{}
}

func (s *blockingAppendStorage) Append(ctx context.Context, event any) error {
	s.entered <- event
	<-s.release
	return s.stubStorageSession.Append(ctx, event)
}

func TestStatusPersistenceReturnsBeforeStorageAppendCompletes(t *testing.T) {
	storageSess := &blockingAppendStorage{
		entered: make(chan any, 1),
		release: make(chan struct{}),
	}
	model := readyModel(t)
	model.Model.Storage = storageSess

	next, cmd := model.Update(session.StatusChanged{Status: "Thinking..."})
	model = next.(Model)

	if model.Progress.Status != "Thinking..." {
		t.Fatalf("status = %q, want Thinking...", model.Progress.Status)
	}
	select {
	case event := <-storageSess.entered:
		t.Fatalf("append ran during Update: %#v", event)
	default:
	}
	if len(storageSess.appends) != 0 {
		t.Fatalf("appends during Update = %#v, want none", storageSess.appends)
	}

	done := make(chan []tea.Msg, 1)
	go func() {
		done <- runSequencePrefix(t, cmd, 1)
	}()
	select {
	case event := <-storageSess.entered:
		if status, ok := event.(storage.Status); !ok || status.Status != "Thinking..." {
			t.Fatalf("append event = %#v, want status", event)
		}
	case <-time.After(time.Second):
		t.Fatal("status persistence command did not start append")
	}
	close(storageSess.release)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("status persistence command did not finish")
	}
	if len(storageSess.appends) != 1 {
		t.Fatalf("appends after command = %#v, want one status append", storageSess.appends)
	}
}

func TestLocalErrorPrintsWithoutProgressError(t *testing.T) {
	model := readyModel(t)

	next, cmd := model.Update(localErrorMsg{err: errors.New("unknown command")})
	model = next.(Model)

	if cmd == nil {
		t.Fatal("expected local error print command")
	}
	if model.Progress.Mode == stateError || model.Progress.LastError != "" {
		t.Fatalf(
			"progress after local error = %v/%q, want no live error",
			model.Progress.Mode,
			model.Progress.LastError,
		)
	}
}

func TestLocalErrorClearsLocalBusyStatus(t *testing.T) {
	for _, status := range []string{
		"Compacting context...",
		"Switching runtime...",
		"Loading session...",
		"Checking provider...",
	} {
		t.Run(status, func(t *testing.T) {
			model := readyModel(t)
			model.Progress.Compacting = isCompactingStatus(status)
			model.Progress.Status = status

			next, cmd := model.Update(localErrorMsg{err: errors.New("operation failed")})
			model = next.(Model)

			if cmd == nil {
				t.Fatal("expected local error print command")
			}
			if model.Progress.Compacting {
				t.Fatal("compacting flag should clear after local error")
			}
			if model.Progress.Status != "" {
				t.Fatalf("status = %q, want cleared", model.Progress.Status)
			}
			if line := ansi.Strip(model.progressLine()); strings.Contains(line, status) {
				t.Fatalf("progress line = %q, want stale local status removed", line)
			}
		})
	}
}

func TestSessionErrorClassifiesProviderRateLimit(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	err := errors.New("error, status code: 429 Too Many Requests: rate limit exceeded")
	next, cmd := model.Update(session.Error{Err: err})
	model = next.(Model)
	runSequencePrefix(t, cmd, 3)

	if !strings.HasPrefix(model.Progress.LastError, "API rate limit: ") {
		t.Fatalf("last error = %q, want API rate limit prefix", model.Progress.LastError)
	}
	if !strings.Contains(model.Progress.LastError, err.Error()) {
		t.Fatalf("last error = %q, want raw provider error", model.Progress.LastError)
	}
	var decision storage.RoutingDecision
	var sys storage.System
	for _, event := range storageSess.appends {
		switch e := event.(type) {
		case storage.RoutingDecision:
			decision = e
		case storage.System:
			sys = e
		}
	}
	if decision.Decision != "stop" || decision.Reason != "rate_limit" {
		t.Fatalf("routing decision = %#v, want stop/rate_limit", decision)
	}
	if decision.StopReason != err.Error() {
		t.Fatalf("stop reason = %q, want raw provider error", decision.StopReason)
	}
	if !strings.Contains(sys.Content, "API rate limit: "+err.Error()) {
		t.Fatalf("system error = %q, want classified raw error", sys.Content)
	}
}

func TestSessionErrorClassifiesProviderQuotaLimit(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	err := errors.New("insufficient_quota: billing hard limit has been reached")
	next, cmd := model.Update(session.Error{Err: err})
	model = next.(Model)
	runSequencePrefix(t, cmd, 2)

	if !strings.HasPrefix(model.Progress.LastError, "API quota or usage limit: ") {
		t.Fatalf("last error = %q, want quota limit prefix", model.Progress.LastError)
	}
	var decision storage.RoutingDecision
	for _, event := range storageSess.appends {
		if e, ok := event.(storage.RoutingDecision); ok {
			decision = e
			break
		}
	}
	if decision.Decision != "stop" || decision.Reason != "quota_limit" {
		t.Fatalf("routing decision = %#v, want stop/quota_limit", decision)
	}
}

func TestTurnStartedClearsStaleToolStatus(t *testing.T) {
	model := readyModel(t)
	model.Progress.Status = "Running bash..."

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)

	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared", model.Progress.Status)
	}
}
