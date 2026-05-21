package app

import (
	"strings"
	"time"

	"github.com/nijaru/ion/internal/session"
)

type turnReducer struct {
	inFlight *InFlightState
	progress *ProgressState
}

func (m *Model) turnReducer() turnReducer {
	return turnReducer{
		inFlight: &m.InFlight,
		progress: &m.Progress,
	}
}

func (r turnReducer) clearActiveState(clearQueued bool) {
	r.inFlight.Thinking = false
	r.inFlight.Pending = nil
	r.inFlight.PendingTools = nil
	r.inFlight.Subagents = make(map[string]*SubagentProgress)
	if clearQueued {
		r.inFlight.QueuedTurns = nil
	}
	r.inFlight.StreamBuf = ""
	r.inFlight.ReasonBuf = ""
	r.inFlight.AgentCommitted = false
	r.inFlight.DrainUntilTurnStarted = false
	r.inFlight.DrainStartedAt = time.Time{}
	r.progress.LastToolUseID = ""
	r.progress.ContextTokens = 0
}

func (r turnReducer) beginDrain(startedAt time.Time) {
	r.inFlight.DrainUntilTurnStarted = true
	r.inFlight.DrainStartedAt = startedAt
}

func (r turnReducer) startSubmit() {
	r.progress.Mode = stateIonizing
	r.progress.Status = ""
	r.progress.LastError = ""
	r.inFlight.Thinking = true
}

func (r turnReducer) rejectSubmit() {
	r.clearActiveState(true)
	r.progress.Compacting = false
	r.progress.Mode = stateReady
	r.progress.Status = ""
	r.progress.StatusUpdatedAt = time.Time{}
	r.progress.LastError = ""
	r.progress.TurnStartedAt = time.Time{}
}

func (r turnReducer) applyTokenUsage(msg session.TokenUsage) {
	r.progress.TokensSent += msg.Input
	r.progress.TokensReceived += msg.Output
	r.progress.ContextTokens += tokenUsageTotal(msg)
	r.progress.TotalCost += msg.Cost
	r.progress.CurrentTurnInput += msg.Input
	r.progress.CurrentTurnOutput += msg.Output
	r.progress.CurrentTurnCost += msg.Cost
}

func (r turnReducer) cancelActiveTurn() {
	r.clearActiveState(true)
	r.beginDrain(time.Now())
	r.progress.Compacting = false
	r.progress.Mode = stateCancelled
	r.progress.Status = ""
	r.progress.StatusUpdatedAt = time.Time{}
}

func (r turnReducer) startTurn(timestamp, startedAt time.Time) {
	r.inFlight.Thinking = true
	r.inFlight.DrainUntilTurnStarted = false
	r.progress.Compacting = false
	r.progress.Mode = stateIonizing
	r.progress.Status = ""
	r.progress.LastError = ""
	r.progress.TurnStartedAt = startedAt
	r.progress.CurrentTurnInput = 0
	r.progress.CurrentTurnOutput = 0
	r.progress.CurrentTurnCost = 0
	r.progress.ContextTokens = 0
	r.progress.BudgetStopReason = ""
	r.inFlight.Pending = &session.Entry{Role: session.Agent, Timestamp: timestamp}
	r.inFlight.PendingTools = nil
	r.inFlight.AgentCommitted = false
}

func (r turnReducer) finishPendingAssistant() (session.Entry, bool, bool) {
	assistantCompleted := r.inFlight.AgentCommitted
	if !r.inFlight.AgentCommitted &&
		r.inFlight.Pending != nil && r.inFlight.Pending.Role == session.Agent &&
		(strings.TrimSpace(r.inFlight.Pending.Content) != "" ||
			strings.TrimSpace(r.inFlight.Pending.Reasoning) != "" ||
			strings.TrimSpace(r.inFlight.ReasonBuf) != "") {
		if strings.TrimSpace(r.inFlight.Pending.Reasoning) == "" {
			r.inFlight.Pending.Reasoning = r.inFlight.ReasonBuf
		}
		entry := *r.inFlight.Pending
		r.clearPendingAssistant()
		return entry, true, true
	}
	if r.inFlight.AgentCommitted {
		r.clearPendingAssistant()
	}
	return session.Entry{}, assistantCompleted, false
}

func (r turnReducer) clearPendingAssistant() {
	r.inFlight.Pending = nil
	r.inFlight.StreamBuf = ""
	r.inFlight.ReasonBuf = ""
}

func (r turnReducer) finishTurnMode(assistantCompleted bool) (session.Entry, bool) {
	switch {
	case r.progress.Mode == stateError:
		r.clearActiveState(true)
		r.progress.Status = ""
	case r.progress.Mode == stateCancelled || r.progress.BudgetStopReason != "":
		r.clearActiveState(true)
		r.progress.Mode = stateCancelled
		r.progress.Status = ""
	case !assistantCompleted:
		r.clearActiveState(true)
		r.progress.Mode = stateError
		r.progress.LastError = "turn finished without assistant response"
		r.progress.Status = ""
		return session.Entry{
			Role:    session.System,
			Content: "Error: turn finished without assistant response",
		}, true
	default:
		r.progress.Mode = stateComplete
	}
	return session.Entry{}, false
}

func (r turnReducer) recordFinishedTurnSummary(finishedAt time.Time) {
	if !r.progress.TurnStartedAt.IsZero() {
		r.progress.LastTurnSummary = turnSummary{
			Elapsed: finishedAt.Sub(r.progress.TurnStartedAt),
			Input:   r.progress.CurrentTurnInput,
			Output:  r.progress.CurrentTurnOutput,
			Cost:    r.progress.CurrentTurnCost,
		}
	}
	r.progress.TurnStartedAt = time.Time{}
}

func (r turnReducer) resetFinishedTurnSummary() {
	r.progress.LastTurnSummary = turnSummary{}
	r.progress.TurnStartedAt = time.Time{}
}

func (r turnReducer) queueTurn(text string) {
	r.inFlight.QueuedTurns = append(r.inFlight.QueuedTurns, text)
}

func (r turnReducer) drainQueuedTurnsText() string {
	if len(r.inFlight.QueuedTurns) == 0 {
		return ""
	}
	queued := strings.Join(r.inFlight.QueuedTurns, "\n")
	r.inFlight.QueuedTurns = nil
	return queued
}

func (r turnReducer) popQueuedTurn() string {
	if len(r.inFlight.QueuedTurns) == 0 {
		return ""
	}
	queued := r.inFlight.QueuedTurns[0]
	r.inFlight.QueuedTurns = r.inFlight.QueuedTurns[1:]
	return queued
}

func (r turnReducer) appendToolOutput(toolUseID, delta string) {
	if entry := r.pendingToolEntry(toolUseID); entry != nil {
		entry.Content += delta
	}
}

func (r turnReducer) completeToolResult(
	toolUseID string,
	msg session.ToolResult,
) (session.Entry, bool) {
	pending := r.pendingToolEntry(toolUseID)
	if pending == nil {
		return session.Entry{}, false
	}
	pending.Content = msg.Result
	pending.IsError = msg.Error != nil
	setEntryTimestamp(pending, msg.Timestamp)
	entry := *pending
	r.clearPendingTool(toolUseID, pending)
	if len(r.inFlight.PendingTools) == 0 {
		r.progress.Mode = stateIonizing
		r.progress.Status = ""
		r.progress.ContextTokens = 0
	}
	return entry, true
}

func (r turnReducer) pendingToolEntry(toolUseID string) *session.Entry {
	if toolUseID != "" {
		return r.inFlight.PendingTools[toolUseID]
	}
	if r.inFlight.Pending != nil && r.inFlight.Pending.Role == session.Tool {
		return r.inFlight.Pending
	}
	return nil
}

func (r turnReducer) clearPendingTool(toolUseID string, entry *session.Entry) {
	if toolUseID != "" {
		delete(r.inFlight.PendingTools, toolUseID)
	}
	if len(r.inFlight.PendingTools) == 0 {
		r.inFlight.PendingTools = nil
	}
	if r.inFlight.Pending == entry {
		r.inFlight.Pending = nil
		for _, id := range sortedKeys(r.inFlight.PendingTools) {
			r.inFlight.Pending = r.inFlight.PendingTools[id]
			break
		}
	}
}
