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
	r.inFlight.Canceling = false
	r.inFlight.StreamBuf = ""
	r.inFlight.StreamChunks = nil
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

func (r turnReducer) drainingUntilTurnStarted() bool {
	return r.inFlight.DrainUntilTurnStarted
}

func (r turnReducer) shouldDrainLateEvent(timestamp time.Time) bool {
	if r.inFlight.DrainStartedAt.IsZero() || timestamp.IsZero() {
		return false
	}
	return !timestamp.After(r.inFlight.DrainStartedAt)
}

func (r turnReducer) finishDrain() {
	r.inFlight.DrainUntilTurnStarted = false
	r.inFlight.DrainStartedAt = time.Time{}
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

func (r turnReducer) streamClosed(now time.Time) (session.Entry, bool) {
	if !r.inFlight.Thinking {
		return session.Entry{}, false
	}
	r.clearActiveState(true)
	r.progress.Compacting = false
	r.progress.Mode = stateError
	r.progress.Status = ""
	r.progress.LastError = "session event stream closed"
	r.recordFinishedTurnSummary(now)
	return session.Entry{
		Role:    session.System,
		Content: "Error: " + r.progress.LastError,
	}, true
}

func (r turnReducer) failTurn(displayErr string, now time.Time) {
	r.clearActiveState(true)
	r.beginDrain(now)
	r.progress.Compacting = false
	r.progress.Mode = stateError
	r.progress.Status = ""
	r.progress.StatusUpdatedAt = time.Time{}
	r.progress.LastError = displayErr
	r.progress.LastTurnSummary = turnSummary{}
	r.recordFinishedTurnSummary(now)
}

func (r turnReducer) clearLocalErrorIfIdle() {
	if r.inFlight.Thinking {
		return
	}
	r.progress.Compacting = false
	if isLocalBusyStatus(r.progress.Status) {
		r.progress.Status = ""
	}
	if r.progress.Mode == stateError {
		r.progress.Mode = stateReady
	}
	r.progress.LastError = ""
}

func (r turnReducer) applyStatusChanged(msg session.StatusChanged) {
	if msg.AgentID == "" {
		r.progress.Status = msg.Status
		r.progress.StatusUpdatedAt = msg.Timestamp
		if r.progress.StatusUpdatedAt.IsZero() {
			r.progress.StatusUpdatedAt = time.Now()
		}
		r.progress.Compacting = isCompactingStatus(msg.Status)
		return
	}
	if p, ok := r.inFlight.Subagents[msg.AgentID]; ok {
		p.Status = msg.Status
	}
}

func (r turnReducer) applyBudgetStop(reason string, timestamp time.Time) (session.Entry, bool) {
	if reason == "" || reason == r.progress.BudgetStopReason {
		return session.Entry{}, false
	}
	r.progress.BudgetStopReason = reason
	if !r.inFlight.Thinking {
		return session.Entry{}, true
	}
	r.clearActiveState(true)
	r.inFlight.Thinking = true
	r.progress.Mode = stateCancelled
	r.progress.Status = ""
	return session.Entry{
		Role:      session.System,
		Timestamp: timestamp,
		Content:   "Canceled: " + reason,
	}, true
}

func (r turnReducer) cancelActiveTurn() {
	r.clearActiveState(true)
	r.inFlight.Thinking = true
	r.inFlight.Canceling = true
	r.beginDrain(time.Now())
	r.progress.Compacting = false
	r.progress.Mode = stateCancelled
	r.progress.Status = ""
	r.progress.StatusUpdatedAt = time.Time{}
}

func (r turnReducer) stopThinking() {
	r.inFlight.Thinking = false
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
	r.inFlight.StreamBuf = ""
	r.inFlight.StreamChunks = nil
	r.inFlight.AgentCommitted = false
}

func (r turnReducer) finishPendingAssistant() (session.Entry, bool, bool) {
	assistantCompleted := r.inFlight.AgentCommitted
	streamContent := r.agentStreamContent()
	if !r.inFlight.AgentCommitted &&
		r.inFlight.Pending != nil && r.inFlight.Pending.Role == session.Agent &&
		(strings.TrimSpace(streamContent) != "" ||
			strings.TrimSpace(r.inFlight.Pending.Reasoning) != "" ||
			strings.TrimSpace(r.inFlight.ReasonBuf) != "") {
		if streamContent != "" {
			r.inFlight.Pending.Content = streamContent
		}
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
	r.inFlight.StreamChunks = nil
	r.inFlight.ReasonBuf = ""
}

func (r turnReducer) finishTurnMode(assistantCompleted bool) (session.Entry, bool) {
	switch {
	case r.progress.Mode == stateError:
		r.clearActiveState(true)
		r.progress.Status = ""
	case r.progress.BudgetStopReason != "":
		r.clearActiveState(true)
		r.progress.Mode = stateCancelled
		r.progress.Status = ""
	case r.progress.Mode == stateCancelled:
		preserveQueued := r.inFlight.Canceling
		r.clearActiveState(!preserveQueued)
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
