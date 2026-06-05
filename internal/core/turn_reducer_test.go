package core

import (
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func newTestState() (*InFlightState, *ProgressState) {
	inFlight := &InFlightState{
		Subagents: make(map[string]*SubagentProgress),
	}
	progress := &ProgressState{}
	return inFlight, progress
}

func TestClearActiveState(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.Thinking = true
	inFlight.Pending = &session.Entry{Role: session.RoleAgent, Content: "test"}
	inFlight.StreamBuf = "buf"
	inFlight.ReasonBuf = "reason"
	inFlight.Canceling = true

	r.ClearActiveState(true)

	if inFlight.Thinking {
		t.Fatal("Thinking should be false")
	}
	if inFlight.Pending != nil {
		t.Fatal("Pending should be nil")
	}
	if inFlight.StreamBuf != "" {
		t.Fatalf("StreamBuf = %q, want empty", inFlight.StreamBuf)
	}
	if inFlight.ReasonBuf != "" {
		t.Fatalf("ReasonBuf = %q, want empty", inFlight.ReasonBuf)
	}
	if inFlight.Canceling {
		t.Fatal("Canceling should be false")
	}
}

func TestClearActiveStatePreservesQueuedWhenFalse(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.QueuedTurns = []string{"queued"}
	inFlight.QueuedSteering = []string{"steer"}

	r.ClearActiveState(false)

	if len(inFlight.QueuedTurns) != 1 {
		t.Fatalf("QueuedTurns len = %d, want 1", len(inFlight.QueuedTurns))
	}
	if len(inFlight.QueuedSteering) != 1 {
		t.Fatalf("QueuedSteering len = %d, want 1", len(inFlight.QueuedSteering))
	}
}

func TestStartSubmitSetsMode(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.StartSubmit()

	if progress.Mode != StateIonizing {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateIonizing)
	}
}

func TestRejectSubmitResetsMode(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.StartSubmit()
	r.RejectSubmit()

	if progress.Mode != StateReady {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateReady)
	}
	if inFlight.Thinking {
		t.Fatal("Thinking should be false after RejectSubmit")
	}
}

func TestStartTurnSetsDrainFields(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	now := time.Now()
	r.StartTurn(now, now)

	if inFlight.Pending == nil {
		t.Fatal("Pending should be set after StartTurn")
	}
	if inFlight.Pending.Role != session.RoleAgent {
		t.Fatalf("Pending.Role = %q, want %q", inFlight.Pending.Role, session.RoleAgent)
	}
	if progress.Mode != StateIonizing {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateIonizing)
	}
}

func TestAppendThinkingDelta(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.AppendThinkingDelta("", "hello ")
	r.AppendThinkingDelta("", "world")

	if inFlight.ReasonBuf != "hello world" {
		t.Fatalf("ReasonBuf = %q, want %q", inFlight.ReasonBuf, "hello world")
	}
}

func TestAppendThinkingDeltaIgnoredAfterCommit(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.AgentCommitted = true
	r.AppendThinkingDelta("", "ignored")

	if inFlight.ReasonBuf != "" {
		t.Fatalf("ReasonBuf = %q, want empty after commit", inFlight.ReasonBuf)
	}
}

func TestAppendAgentDelta(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.Pending = &session.Entry{Role: session.RoleAgent}
	r.AppendAgentDelta("", "hello", time.Now())

	if inFlight.StreamBuf != "hello" {
		t.Fatalf("StreamBuf = %q, want %q", inFlight.StreamBuf, "hello world")
	}
}

func TestCommitAgentMessage(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.Pending = &session.Entry{Role: session.RoleAgent}
	inFlight.ReasonBuf = "reasoning"

	entry, ok := r.CommitAgentMessage(session.AgentMessageEvent{
		Base:    session.Base{Timestamp: time.Now()},
		Message: "hello",
	})
	if !ok {
		t.Fatal("CommitAgentMessage should succeed")
	}
	if entry.Content != "hello" {
		t.Fatalf("Content = %q, want %q", entry.Content, "hello")
	}
	if entry.Reasoning != "reasoning" {
		t.Fatalf("Reasoning = %q, want %q", entry.Reasoning, "reasoning")
	}
	if inFlight.ReasonBuf != "" {
		t.Fatalf("ReasonBuf should be cleared after commit")
	}
}

func TestCommitAgentMessageEmptyFails(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.Pending = &session.Entry{Role: session.RoleAgent}

	_, ok := r.CommitAgentMessage(session.AgentMessageEvent{
		Base:    session.Base{Timestamp: time.Now()},
		Message: "",
	})
	if ok {
		t.Fatal("CommitAgentMessage with empty content should fail")
	}
}

func TestCancelActiveTurn(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.Pending = &session.Entry{Role: session.RoleAgent, Content: "partial"}
	inFlight.Thinking = true

	now := time.Now()
	decision := r.CancelActiveTurn("user cancel", now)

	// CancelActiveTurn sets Thinking based on decision, not necessarily false
	if !decision.Canceling {
		t.Fatal("Canceling should be true")
	}
	if progress.Mode != StateCancelled {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateCancelled)
	}
}

func TestFailTurn(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.Mode = StateStreaming
	inFlight.Thinking = true

	now := time.Now()
	r.FailTurn("connection error", now)

	if progress.Mode != StateError {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateError)
	}
	if progress.LastError != "connection error" {
		t.Fatalf("LastError = %q, want %q", progress.LastError, "connection error")
	}
	if inFlight.Thinking {
		t.Fatal("Thinking should be false after fail")
	}
}

func TestStreamClosedWhenThinking(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	// StreamClosed is terminal only when Thinking is true
	inFlight.Thinking = true

	now := time.Now()
	entry, ok := r.StreamClosed(now)

	if !ok {
		t.Fatal("StreamClosed should be terminal when Thinking")
	}
	if entry.Content == "" {
		t.Fatal("StreamClosed entry should have content")
	}
	if progress.Mode != StateError {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateError)
	}
}

func TestStreamClosedNotThinking(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	// When not thinking, stream close is not terminal
	inFlight.Thinking = false

	now := time.Now()
	_, ok := r.StreamClosed(now)

	if ok {
		t.Fatal("StreamClosed should not be terminal when not thinking")
	}
}

func TestApplyBudgetStop(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.Pending = &session.Entry{Role: session.RoleAgent, Content: "content"}

	now := time.Now()
	_, ok := r.ApplyBudgetStop("token limit", now)

	// ApplyBudgetStop may return empty entry depending on decision
	if !ok {
		t.Fatal("ApplyBudgetStop should return ok")
	}
	if progress.BudgetStopReason != "token limit" {
		t.Fatalf("BudgetStopReason = %q, want %q", progress.BudgetStopReason, "token limit")
	}
}

func TestFinishTurnModePreservesError(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.Mode = StateError
	progress.LastError = "some error"

	entry, ok := r.FinishTurnMode(false)

	// PreserveError returns empty entry, false
	if ok {
		t.Fatal("FinishTurnMode should return false for PreserveError")
	}
	if entry.Content != "" {
		t.Fatalf("Content should be empty for PreserveError, got %q", entry.Content)
	}
	if progress.Mode != StateError {
		t.Fatalf("Mode = %d, want %d (preserve error)", progress.Mode, StateError)
	}
}

func TestFinishTurnModeCancelled(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.Mode = StateCancelled

	entry, ok := r.FinishTurnMode(false)

	// UserCancel returns empty entry, false
	if ok {
		t.Fatal("FinishTurnMode should return false for UserCancel")
	}
	if entry.Content != "" {
		t.Fatalf("Content should be empty for UserCancel, got %q", entry.Content)
	}
	if progress.Mode != StateCancelled {
		t.Fatalf("Mode = %d, want %d (preserve cancelled)", progress.Mode, StateCancelled)
	}
}

func TestFinishTurnModeSuccess(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.Mode = StateStreaming

	entry, ok := r.FinishTurnMode(true)

	// Complete returns empty entry, false
	if ok {
		t.Fatal("FinishTurnMode should return false for Complete")
	}
	if entry.Content != "" {
		t.Fatalf("Content should be empty for Complete, got %q", entry.Content)
	}
	if progress.Mode != StateComplete {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateComplete)
	}
}

func TestFinishTurnModeMissingAgent(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.Mode = StateStreaming

	entry, ok := r.FinishTurnMode(false)

	// MissingAgent returns error entry, true
	if !ok {
		t.Fatal("FinishTurnMode should return true for MissingAgent")
	}
	if entry.Content == "" {
		t.Fatal("MissingAgent should have error content")
	}
	if progress.Mode != StateError {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateError)
	}
}

func TestQueueAndPopTurn(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.QueueTurn("first")
	r.QueueTurn("second")

	if len(inFlight.QueuedTurns) != 2 {
		t.Fatalf("QueuedTurns len = %d, want 2", len(inFlight.QueuedTurns))
	}

	first := r.PopQueuedTurn()
	if first != "first" {
		t.Fatalf("PopQueuedTurn = %q, want %q", first, "first")
	}

	second := r.PopQueuedTurn()
	if second != "second" {
		t.Fatalf("PopQueuedTurn = %q, want %q", second, "second")
	}

	empty := r.PopQueuedTurn()
	if empty != "" {
		t.Fatalf("PopQueuedTurn = %q, want empty", empty)
	}
}

func TestStartToolCall(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	id := r.StartToolCall("call-1", time.Now(), "grep")

	if len(inFlight.PendingTools) != 1 {
		t.Fatalf("PendingTools len = %d, want 1", len(inFlight.PendingTools))
	}
	entry := inFlight.PendingTools[id]
	if entry.Title != "grep" {
		t.Fatalf("Title = %q, want %q", entry.Title, "grep")
	}
	if progress.LastToolUseID != id {
		t.Fatalf("LastToolUseID = %q, want %q", progress.LastToolUseID, id)
	}
}

func TestCompleteToolResult(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	id := r.StartToolCall("call-1", time.Now(), "grep")

	entry, ok := r.CompleteToolResult(id, session.ToolResultEvent{
		Base:   session.Base{Timestamp: time.Now()},
		Result: "file.go:10",
	})
	if !ok {
		t.Fatal("CompleteToolResult should succeed")
	}
	if entry.Content != "file.go:10" {
		t.Fatalf("Content = %q, want %q", entry.Content, "file.go:10")
	}
}

func TestCompleteToolResultUnknown(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	_, ok := r.CompleteToolResult("unknown", session.ToolResultEvent{
		Base:   session.Base{Timestamp: time.Now()},
		Result: "output",
	})
	if ok {
		t.Fatal("CompleteToolResult with unknown ID should fail")
	}
}

func TestRequestAndCompleteChild(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	child := r.RequestChild("research", "find papers")
	if child == nil {
		t.Fatal("RequestChild should return progress")
	}
	if child.Name != "research" {
		t.Fatalf("Name = %q, want %q", child.Name, "research")
	}

	started := r.StartChild("research")
	if !started {
		t.Fatal("StartChild should succeed")
	}

	_ = r.AppendChildDelta("research", "found ")
	_ = r.AppendChildDelta("research", "results")

	r.CompleteChild("research", "done", time.Now())

	if len(inFlight.Subagents) != 0 {
		t.Fatalf("Subagents len = %d, want 0 after complete", len(inFlight.Subagents))
	}
}

func TestBlockChild(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.RequestChild("worker", "task")
	r.StartChild("worker")

	ok := r.BlockChild("worker", "need approval")
	if !ok {
		t.Fatal("BlockChild should succeed")
	}

	child := inFlight.Subagents["worker"]
	if child.Status != "Blocked" {
		t.Fatalf("Status = %q, want %q", child.Status, "Blocked")
	}
}

func TestFailChild(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.RequestChild("worker", "task")
	r.StartChild("worker")

	_, ok := r.FailChild("worker", "timeout", time.Now())
	if !ok {
		t.Fatal("FailChild should succeed")
	}

	if len(inFlight.Subagents) != 0 {
		t.Fatalf("Subagents len = %d, want 0 after fail", len(inFlight.Subagents))
	}
}

func TestCancelChild(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.RequestChild("worker", "task")
	r.StartChild("worker")

	_, ok := r.CancelChild("worker", "user cancel", time.Now())
	if !ok {
		t.Fatal("CancelChild should succeed")
	}

	if len(inFlight.Subagents) != 0 {
		t.Fatalf("Subagents len = %d, want 0 after cancel", len(inFlight.Subagents))
	}
}

func TestDrainLifecycle(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	now := time.Now()
	r.BeginDrain(now)

	if !r.DrainingUntilTurnStarted() {
		t.Fatal("Should be draining after BeginDrain")
	}

	r.FinishDrain()

	if r.DrainingUntilTurnStarted() {
		t.Fatal("Should not be draining after FinishDrain")
	}
}

func TestRecordFinishedTurnSummary(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.TurnStartedAt = time.Now().Add(-time.Second)
	progress.CurrentTurnInput = 100
	progress.CurrentTurnOutput = 50
	progress.CurrentTurnCost = 0.01

	now := time.Now()
	r.RecordFinishedTurnSummary(now)

	if progress.LastTurnSummary.Input != 100 {
		t.Fatalf("Input = %d, want 100", progress.LastTurnSummary.Input)
	}
	if progress.LastTurnSummary.Output != 50 {
		t.Fatalf("Output = %d, want 50", progress.LastTurnSummary.Output)
	}

	r.ResetFinishedTurnSummary()

	if progress.LastTurnSummary.Input != 0 {
		t.Fatal("TurnSummary should be reset")
	}
}

func TestApplyTokenUsage(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	r.ApplyTokenUsage(session.TokenUsageEvent{
		Base:   session.Base{Timestamp: time.Now()},
		Input:  100,
		Output: 50,
		Cost:   0.005,
	})

	if progress.TokensSent != 100 {
		t.Fatalf("TokensSent = %d, want 100", progress.TokensSent)
	}
	if progress.TokensReceived != 50 {
		t.Fatalf("TokensReceived = %d, want 50", progress.TokensReceived)
	}
	if progress.TotalCost != 0.005 {
		t.Fatalf("TotalCost = %f, want 0.005", progress.TotalCost)
	}
}

func TestClearLocalErrorIfIdle(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.Mode = StateError
	progress.LastError = "some error"

	r.ClearLocalErrorIfIdle()

	if progress.Mode != StateReady {
		t.Fatalf("Mode = %d, want %d", progress.Mode, StateReady)
	}
	if progress.LastError != "" {
		t.Fatalf("LastError = %q, want empty", progress.LastError)
	}
}

func TestClearLocalErrorIfIdleNotInError(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	progress.Mode = StateStreaming
	progress.LastError = "error"

	r.ClearLocalErrorIfIdle()

	// ClearLocalErrorIfIdle always clears LastError
	if progress.LastError != "" {
		t.Fatalf("LastError = %q, want empty (always cleared)", progress.LastError)
	}
}

func TestSortedKeys(t *testing.T) {
	m := map[string]int{"c": 3, "a": 1, "b": 2}
	keys := SortedKeys(m)

	if len(keys) != 3 {
		t.Fatalf("len = %d, want 3", len(keys))
	}
	if keys[0] != "a" || keys[1] != "b" || keys[2] != "c" {
		t.Fatalf("keys = %v, want [a b c]", keys)
	}
}

func TestStopThinking(t *testing.T) {
	inFlight, progress := newTestState()
	r := NewTurnReducer(inFlight, progress)

	inFlight.Thinking = true
	r.StopThinking()

	if inFlight.Thinking {
		t.Fatal("Thinking should be false after StopThinking")
	}
}
