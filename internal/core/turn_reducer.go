package core

import (
	"slices"
	"strings"
	"time"

	"github.com/nijaru/ion/session"
)

// TurnReducer is a state machine that manages turn lifecycle, streaming,
// tool results, subagent progress, and cancellation settlement.
type TurnReducer struct {
	InFlight *InFlightState
	Progress *ProgressState
}

func NewTurnReducer(inFlight *InFlightState, progress *ProgressState) TurnReducer {
	return TurnReducer{InFlight: inFlight, Progress: progress}
}

func (r TurnReducer) ClearActiveState(clearQueued bool) {
	r.InFlight.Thinking = false
	r.InFlight.Pending = nil
	r.InFlight.PendingTools = nil
	r.InFlight.Subagents = make(map[string]*SubagentProgress)
	if clearQueued {
		r.InFlight.QueuedSteering = nil
		r.InFlight.QueuedTurns = nil
	}
	r.InFlight.Canceling = false
	r.InFlight.StreamBuf = ""
	r.InFlight.StreamChunks = nil
	r.InFlight.ReasonBuf = ""
	r.InFlight.AgentCommitted = false
	if clearQueued {
		r.InFlight.QueuedTurnsBackendOwned = false
	}
	r.InFlight.DrainUntilTurnStarted = false
	r.InFlight.DrainStartedAt = time.Time{}
	r.Progress.LastToolUseID = ""
	r.Progress.ContextTokens = 0
}

func (r TurnReducer) BeginDrain(startedAt time.Time) {
	r.InFlight.DrainUntilTurnStarted = true
	r.InFlight.DrainStartedAt = startedAt
}

func (r TurnReducer) DrainingUntilTurnStarted() bool {
	return r.InFlight.DrainUntilTurnStarted
}

func (r TurnReducer) FinishDrain() {
	r.InFlight.DrainUntilTurnStarted = false
	r.InFlight.DrainStartedAt = time.Time{}
}

func (r TurnReducer) StartSubmit() {
	r.Progress.Mode = StateIonizing
	r.Progress.Status = ""
	r.Progress.LastError = ""
	r.InFlight.Thinking = true
}

func (r TurnReducer) RejectSubmit() {
	r.ClearActiveState(true)
	r.Progress.Compacting = false
	r.Progress.Mode = StateReady
	r.Progress.Status = ""
	r.Progress.StatusUpdatedAt = time.Time{}
	r.Progress.LastError = ""
	r.Progress.TurnStartedAt = time.Time{}
}

func (r TurnReducer) ApplyTokenUsage(msg session.AgentMessage) {
	total := msg.TotalTokens
	if total == 0 {
		total = msg.InputTokens + msg.OutputTokens
	}
	r.Progress.TokensSent += msg.InputTokens
	r.Progress.TokensReceived += msg.OutputTokens
	r.Progress.ContextTokens += total
	r.Progress.TotalCost += msg.Cost
	r.Progress.CurrentTurnInput += msg.InputTokens
	r.Progress.CurrentTurnOutput += msg.OutputTokens
	r.Progress.CurrentTurnCost += msg.Cost
}

func (r TurnReducer) StreamClosed(now time.Time) (session.Entry, bool) {
	decision := session.DecideStreamClosure(session.StreamClosureInput{
		Thinking: r.InFlight.Thinking,
	})
	if !decision.Terminal {
		return session.Entry{}, false
	}
	r.ClearActiveState(true)
	r.Progress.Compacting = false
	r.Progress.Mode = StateError
	r.Progress.Status = ""
	r.Progress.LastError = decision.DisplayError
	r.RecordFinishedTurnSummary(now)
	entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
	return entry, true
}

func (r TurnReducer) FailTurn(displayErr string, now time.Time) {
	r.ClearActiveState(true)
	r.BeginDrain(now)
	r.Progress.Compacting = false
	r.Progress.Mode = StateError
	r.Progress.Status = ""
	r.Progress.StatusUpdatedAt = time.Time{}
	r.Progress.LastError = displayErr
	r.Progress.LastTurnSummary = TurnSummary{}
	r.RecordFinishedTurnSummary(now)
}

func (r TurnReducer) ClearLocalErrorIfIdle() {
	if r.InFlight.Thinking {
		return
	}
	r.Progress.Compacting = false
	if IsLocalBusyStatus(r.Progress.Status) {
		r.Progress.Status = ""
	}
	if r.Progress.Mode == StateError {
		r.Progress.Mode = StateReady
	}
	r.Progress.LastError = ""
}

func (r TurnReducer) ApplyStatusChanged(msg session.StatusChange) session.StatusChangeDecision {
	decision := session.DecideStatusChange(session.StatusChangeInput{
		AgentID:   msg.AgentID,
		Status:    msg.Status,
		Timestamp: msg.Timestamp,
		Now:       time.Now(),
	})
	if decision.Root {
		r.Progress.Status = decision.Status
		r.Progress.StatusUpdatedAt = decision.StatusUpdatedAt
		r.Progress.Compacting = decision.Compacting
		return decision
	}
	if p, ok := r.InFlight.Subagents[msg.AgentID]; ok {
		p.Status = msg.Status
	}
	return decision
}

func (r TurnReducer) ApplyBudgetStop(reason string, timestamp time.Time) (session.Entry, bool) {
	decision := session.DecideBudgetStopSettlement(session.BudgetStopSettlementInput{
		Reason:         reason,
		ExistingReason: r.Progress.BudgetStopReason,
		Thinking:       r.InFlight.Thinking,
	})
	if decision.Action == session.BudgetStopIgnore {
		return session.Entry{}, false
	}
	r.Progress.BudgetStopReason = decision.Reason
	if decision.Action == session.BudgetStopRecord {
		return session.Entry{}, true
	}
	r.ClearActiveState(true)
	r.InFlight.Thinking = true
	r.Progress.Mode = StateCancelled
	r.Progress.Status = ""
	entry, _ := session.EntrySystem(decision.EntryContent, timestamp)
	return entry, true
}

func (r TurnReducer) CancelActiveTurn(
	reason string,
	now time.Time,
) session.CancelStartDecision {
	decision := session.DecideCancelStart(session.CancelStartInput{
		Reason: reason,
		Now:    now,
	})
	r.ClearActiveState(decision.ClearQueued)
	r.InFlight.Thinking = decision.Thinking
	r.InFlight.Canceling = decision.Canceling
	r.BeginDrain(decision.DrainStartedAt)
	r.Progress.Compacting = false
	r.Progress.Mode = StateCancelled
	r.Progress.Status = ""
	r.Progress.StatusUpdatedAt = time.Time{}
	return decision
}

func (r TurnReducer) StopThinking() {
	r.InFlight.Thinking = false
}

func (r TurnReducer) StartTurn(timestamp, startedAt time.Time) {
	r.InFlight.Thinking = true
	r.InFlight.DrainUntilTurnStarted = false
	r.Progress.Compacting = false
	r.Progress.Mode = StateIonizing
	r.Progress.Status = ""
	r.Progress.LastError = ""
	r.Progress.TurnStartedAt = startedAt
	r.Progress.CurrentTurnInput = 0
	r.Progress.CurrentTurnOutput = 0
	r.Progress.CurrentTurnCost = 0
	r.Progress.ContextTokens = 0
	r.Progress.BudgetStopReason = ""
	r.InFlight.Pending = &session.Entry{Role: session.RoleAgent, Timestamp: timestamp}
	r.InFlight.PendingTools = nil
	r.InFlight.StreamBuf = ""
	r.InFlight.StreamChunks = nil
	r.InFlight.AgentCommitted = false
}

func (r TurnReducer) FinishPendingAssistant() (session.Entry, bool, bool) {
	assistantCompleted := r.InFlight.AgentCommitted
	streamContent := r.AgentStreamContent()
	if !r.InFlight.AgentCommitted &&
		r.InFlight.Pending != nil && r.InFlight.Pending.Role == session.RoleAgent &&
		(strings.TrimSpace(streamContent) != "" ||
			strings.TrimSpace(r.InFlight.Pending.Reasoning) != "" ||
			strings.TrimSpace(r.InFlight.ReasonBuf) != "") {
		if streamContent != "" {
			r.InFlight.Pending.Content = streamContent
		}
		if strings.TrimSpace(r.InFlight.Pending.Reasoning) == "" {
			r.InFlight.Pending.Reasoning = r.InFlight.ReasonBuf
		}
		entry, ok := session.EntryAgent(
			r.InFlight.Pending.Content,
			r.InFlight.Pending.Reasoning,
			r.InFlight.Pending.Timestamp,
		)
		r.ClearPendingAssistant()
		return entry, ok, ok
	}
	if r.InFlight.AgentCommitted {
		r.ClearPendingAssistant()
	}
	return session.Entry{}, assistantCompleted, false
}

func (r TurnReducer) ClearPendingAssistant() {
	r.InFlight.Pending = nil
	r.InFlight.StreamBuf = ""
	r.InFlight.StreamChunks = nil
	r.InFlight.ReasonBuf = ""
}

func (r TurnReducer) FinishTurnMode(assistantCompleted bool) (session.Entry, bool) {
	decision := session.DecideTurnFinishMode(session.TurnFinishModeInput{
		HadError:           r.Progress.Mode == StateError,
		BudgetStopReason:   r.Progress.BudgetStopReason,
		Canceled:           r.Progress.Mode == StateCancelled,
		Canceling:          r.InFlight.Canceling,
		AssistantCompleted: assistantCompleted,
	})
	switch decision.Action {
	case session.TurnFinishPreserveError:
		r.ClearActiveState(decision.ClearQueued)
		r.Progress.Status = ""
	case session.TurnFinishBudgetCancel:
		r.ClearActiveState(decision.ClearQueued)
		r.Progress.Mode = StateCancelled
		r.Progress.Status = ""
	case session.TurnFinishUserCancel:
		r.ClearActiveState(decision.ClearQueued)
		r.Progress.Mode = StateCancelled
		r.Progress.Status = ""
	case session.TurnFinishMissingAgent:
		r.ClearActiveState(decision.ClearQueued)
		r.Progress.Mode = StateError
		r.Progress.LastError = decision.DisplayError
		r.Progress.Status = ""
		entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
		return entry, true
	case session.TurnFinishComplete:
		r.Progress.Mode = StateComplete
	}
	return session.Entry{}, false
}

func (r TurnReducer) RecordFinishedTurnSummary(finishedAt time.Time) {
	if !r.Progress.TurnStartedAt.IsZero() {
		r.Progress.LastTurnSummary = TurnSummary{
			Elapsed: finishedAt.Sub(r.Progress.TurnStartedAt),
			Input:   r.Progress.CurrentTurnInput,
			Output:  r.Progress.CurrentTurnOutput,
			Cost:    r.Progress.CurrentTurnCost,
		}
	}
	r.Progress.TurnStartedAt = time.Time{}
}

func (r TurnReducer) ResetFinishedTurnSummary() {
	r.Progress.LastTurnSummary = TurnSummary{}
	r.Progress.TurnStartedAt = time.Time{}
}

func (r TurnReducer) QueueTurn(text string) {
	r.InFlight.QueuedTurns = append(r.InFlight.QueuedTurns, text)
	r.InFlight.QueuedTurnsBackendOwned = false
}

func (r TurnReducer) SetBackendQueuedTurns(texts []string) {
	r.SetBackendQueuedInput(nil, texts)
}

func (r TurnReducer) SetBackendQueuedInput(steering, followUp []string) {
	r.InFlight.QueuedSteering = append([]string(nil), steering...)
	r.InFlight.QueuedTurns = append([]string(nil), followUp...)
	r.InFlight.QueuedTurnsBackendOwned = len(steering) > 0 || len(followUp) > 0
}

func (r TurnReducer) ClearQueuedTurns() {
	r.InFlight.QueuedSteering = nil
	r.InFlight.QueuedTurns = nil
	r.InFlight.QueuedTurnsBackendOwned = false
}

func (r TurnReducer) PopQueuedTurn() string {
	decision := session.DecideTurnSettlement(session.TurnSettlementInput{
		BackendOwnedQueued: r.InFlight.QueuedTurnsBackendOwned,
		LocalQueuedTurns:   r.InFlight.QueuedTurns,
	})
	if decision.Action != session.TurnSettlementSubmitLocal {
		return ""
	}
	r.InFlight.QueuedTurns = r.InFlight.QueuedTurns[1:]
	return decision.Text
}

func (r TurnReducer) FinishTurnDispatch() session.TurnFinishedDispatchDecision {
	decision := session.DecideTurnFinishedDispatch(session.TurnFinishedDispatchInput{
		BackendOwnedQueued: r.InFlight.QueuedTurnsBackendOwned,
		LocalQueuedTurns:   r.InFlight.QueuedTurns,
	})
	if decision.Action == session.TurnFinishedDispatchSubmitLocal {
		r.InFlight.QueuedTurns = r.InFlight.QueuedTurns[1:]
	}
	return decision
}

// IsLocalBusyStatus reports whether the status string represents an
// internal busy indicator that should be cleared when idle.
func IsLocalBusyStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	return trimmed == "Switching runtime..." ||
		trimmed == "Saving runtime settings..." ||
		trimmed == "Loading session..." ||
		trimmed == "Checking provider..." ||
		trimmed == "Saving provider setup..." ||
		trimmed == "Loading settings..." ||
		trimmed == "Saving settings..." ||
		IsCompactingStatus(trimmed)
}

// IsCompactingStatus reports whether the status represents active compaction.
func IsCompactingStatus(status string) bool {
	return strings.TrimSpace(status) == "Compacting context..."
}

// --- Subagent / child management ---

func (r TurnReducer) RequestChild(name, intent string) *SubagentProgress {
	p := &SubagentProgress{
		ID:     name,
		Name:   name,
		Intent: intent,
		Status: "Requested",
	}
	if r.InFlight.Subagents == nil {
		r.InFlight.Subagents = make(map[string]*SubagentProgress)
	}
	r.InFlight.Subagents[name] = p
	r.Progress.Mode = StateWorking
	return p
}

func (r TurnReducer) StartChild(name string) bool {
	if p, ok := r.InFlight.Subagents[name]; ok {
		p.Status = "Started"
		r.Progress.Mode = StateWorking
		return true
	}
	return false
}

func (r TurnReducer) AppendChildDelta(name, delta string) bool {
	if p, ok := r.InFlight.Subagents[name]; ok {
		p.Output += delta
		return true
	}
	return false
}

func (r TurnReducer) CommitSubagentMessage(
	id, message string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.InFlight.Subagents[id]
	if !ok {
		return session.Entry{}, false
	}
	content := p.Output
	if message != "" {
		content = message
	}
	entry, _ := session.EntrySubagent(p.Name, "Completed: "+content, false, timestamp)
	entry.Reasoning = p.Reasoning
	delete(r.InFlight.Subagents, id)
	r.SettleChildProgress()
	return entry, true
}

func (r TurnReducer) CompleteChild(
	name, result string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.InFlight.Subagents[name]
	if !ok {
		return session.Entry{}, false
	}
	p.Status = "Completed"
	p.Output = result
	entry, _ := session.EntrySubagent(p.Name, "Completed: "+p.Output, false, timestamp)
	delete(r.InFlight.Subagents, name)
	r.SettleChildProgress()
	return entry, true
}

func (r TurnReducer) BlockChild(name, reason string) bool {
	p, ok := r.InFlight.Subagents[name]
	if !ok {
		return false
	}
	p.Status = "Blocked"
	p.Output = "BLOCKED: " + reason
	r.Progress.Mode = StateBlocked
	r.InFlight.Thinking = false
	return true
}

func (r TurnReducer) FailChild(
	name, err string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.InFlight.Subagents[name]
	if !ok {
		return session.Entry{}, false
	}
	p.Status = "Failed"
	p.Output = "ERROR: " + err
	entry, _ := session.EntrySubagent(p.Name, "Failed: "+err, true, timestamp)
	delete(r.InFlight.Subagents, name)
	r.Progress.Mode = StateError
	r.Progress.LastError = "Subagent failed: " + err
	return entry, true
}

func (r TurnReducer) CancelChild(
	name, reason string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.InFlight.Subagents[name]
	if !ok {
		return session.Entry{}, false
	}
	p.Status = "Canceled"
	p.Output = ChildCanceledContent(reason)
	entry, _ := session.EntrySubagent(p.Name, p.Output, false, timestamp)
	delete(r.InFlight.Subagents, name)
	r.SettleChildProgress()
	return entry, true
}

func (r TurnReducer) SettleChildProgress() {
	r.Progress.Status = ""
	switch {
	case len(r.InFlight.Subagents) > 0:
		r.Progress.Mode = StateWorking
	case r.InFlight.Thinking:
		r.Progress.Mode = StateIonizing
	default:
		r.Progress.Mode = StateComplete
	}
}

// ChildCanceledContent returns a display string for a canceled subagent.
func ChildCanceledContent(reason string) string {
	if reason == "" {
		return "Canceled"
	}
	return "Canceled: " + reason
}

// --- Message processing ---

func (r TurnReducer) AppendThinkingDelta(agentID, delta string) {
	if agentID == "" {
		if r.InFlight.AgentCommitted {
			return
		}
		r.InFlight.ReasonBuf += delta
		return
	}
	if p, ok := r.InFlight.Subagents[agentID]; ok {
		p.Reasoning += delta
	}
}

func (r TurnReducer) AppendAgentDelta(agentID, delta string, timestamp time.Time) {
	if agentID == "" {
		if r.InFlight.AgentCommitted {
			return
		}
		r.Progress.Mode = StateStreaming
		if r.InFlight.Pending == nil || r.InFlight.Pending.Role != session.RoleAgent {
			r.InFlight.Pending = &session.Entry{
				Role:      session.RoleAgent,
				Timestamp: timestamp,
			}
		}
		r.InFlight.StreamChunks = append(r.InFlight.StreamChunks, delta)
		if r.InFlight.StreamBuf == "" {
			r.InFlight.StreamBuf = delta
		}
		if r.InFlight.Pending.Content == "" {
			r.InFlight.Pending.Content = delta
		}
		return
	}
	if p, ok := r.InFlight.Subagents[agentID]; ok {
		p.Output += delta
	}
}

func (r TurnReducer) CommitAgentMessage(msg session.AgentMessage) (session.Entry, bool) {
	if msg.AgentID != "" {
		return session.Entry{}, false
	}
	if r.InFlight.Pending != nil && r.InFlight.Pending.Role == session.RoleAgent {
		if msg.Message != "" {
			r.InFlight.Pending.Content = msg.Message
		} else if streamContent := r.AgentStreamContent(); streamContent != "" {
			r.InFlight.Pending.Content = streamContent
		}
		r.InFlight.Pending.Reasoning = r.InFlight.ReasonBuf
		if msg.Reasoning != "" {
			r.InFlight.Pending.Reasoning = msg.Reasoning
		}
		session.SetTimestamp(r.InFlight.Pending, msg.Timestamp)
		entry := *r.InFlight.Pending
		r.ClearPendingAssistant()
		entry, ok := session.EntryAgent(entry.Content, entry.Reasoning, entry.Timestamp)
		if !ok {
			return session.Entry{}, false
		}
		r.InFlight.AgentCommitted = true
		return entry, true
	}
	reasoning := r.InFlight.ReasonBuf
	if msg.Reasoning != "" {
		reasoning = msg.Reasoning
	}
	r.InFlight.StreamBuf = ""
	r.InFlight.StreamChunks = nil
	r.InFlight.ReasonBuf = ""
	entry, ok := session.EntryAgent(msg.Message, reasoning, msg.Timestamp)
	if !ok {
		return session.Entry{}, false
	}
	r.InFlight.AgentCommitted = true
	return entry, true
}

func (r TurnReducer) AgentStreamContent() string {
	if len(r.InFlight.StreamChunks) > 0 {
		return strings.Join(r.InFlight.StreamChunks, "")
	}
	if r.InFlight.Pending != nil && r.InFlight.Pending.Role == session.RoleAgent {
		return r.InFlight.Pending.Content
	}
	return r.InFlight.StreamBuf
}

func (r TurnReducer) AgentStreamEmpty() bool {
	return len(r.InFlight.StreamChunks) == 0 &&
		r.InFlight.StreamBuf == "" &&
		(r.InFlight.Pending == nil || r.InFlight.Pending.Content == "")
}

// --- Tool processing ---

func (r TurnReducer) AppendToolOutput(toolUseID, delta string, snapshot bool) {
	if entry := r.PendingToolEntry(toolUseID); entry != nil {
		if snapshot {
			entry.Content = delta
			return
		}
		entry.Content += delta
	}
}

func (r TurnReducer) StartToolCall(
	toolUseID string,
	timestamp time.Time,
	title string,
) string {
	r.Progress.Mode = StateWorking
	r.Progress.LastToolUseID = toolUseID
	if r.Progress.LastToolUseID == "" {
		r.Progress.LastToolUseID = session.ShortID()
	}
	projected, _ := session.Tool(title, "", false, timestamp)
	entry := &projected
	if r.InFlight.PendingTools == nil {
		r.InFlight.PendingTools = make(map[string]*session.Entry)
	}
	r.InFlight.PendingTools[r.Progress.LastToolUseID] = entry
	if r.InFlight.Pending == nil || r.InFlight.Pending.Role == session.RoleTool ||
		(r.InFlight.Pending.Role == session.RoleAgent &&
			r.AgentStreamEmpty() &&
			r.InFlight.ReasonBuf == "") {
		r.InFlight.Pending = entry
	}
	return r.Progress.LastToolUseID
}

func (r TurnReducer) CompleteToolResult(
	toolUseID string,
	msg session.ToolCallEnd,
) (session.Entry, bool) {
	pending := r.PendingToolEntry(toolUseID)
	if pending == nil {
		return session.Entry{}, false
	}
	pending.Content = msg.Result
	pending.IsError = msg.Error != nil
	session.SetTimestamp(pending, msg.Timestamp)
	entry, _ := session.Tool(pending.Title, pending.Content, pending.IsError, pending.Timestamp)
	r.ClearPendingTool(toolUseID, pending)
	if len(r.InFlight.PendingTools) == 0 {
		r.Progress.Mode = StateIonizing
		r.Progress.Status = ""
		r.Progress.ContextTokens = 0
	}
	return entry, true
}

func (r TurnReducer) PendingToolEntry(toolUseID string) *session.Entry {
	if toolUseID != "" {
		return r.InFlight.PendingTools[toolUseID]
	}
	if r.InFlight.Pending != nil && r.InFlight.Pending.Role == session.RoleTool {
		return r.InFlight.Pending
	}
	return nil
}

func (r TurnReducer) ClearPendingTool(toolUseID string, entry *session.Entry) {
	if toolUseID != "" {
		delete(r.InFlight.PendingTools, toolUseID)
	}
	if len(r.InFlight.PendingTools) == 0 {
		r.InFlight.PendingTools = nil
	}
	if r.InFlight.Pending == entry {
		r.InFlight.Pending = nil
		for _, id := range SortedKeys(r.InFlight.PendingTools) {
			r.InFlight.Pending = r.InFlight.PendingTools[id]
			break
		}
	}
}

// SortedKeys returns the sorted keys of a map.
func SortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	return keys
}
