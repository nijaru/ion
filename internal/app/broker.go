package app

import (
	"context"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/privacy"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type localErrorMsg struct {
	err error
}

func (m Model) awaitSessionEvent() tea.Cmd {
	generation := m.Model.EventGeneration
	events := m.Model.Session.Events()
	return func() tea.Msg {
		ev, ok := <-events
		if !ok {
			return streamClosedMsg{generation: generation}
		}
		return sessionEventMsg{generation: generation, event: ev}
	}
}

// handleSessionEvent processes events from the agent session channel.
func (m Model) handleSessionEvent(ev session.Event) (Model, tea.Cmd) {
	if m.InFlight.DrainUntilTurnStarted {
		switch msg := ev.(type) {
		case session.UserMessage:
			if m.shouldDrainLateEvent(msg.Timestamp) {
				return m, m.awaitSessionEvent()
			}
			m.InFlight.DrainUntilTurnStarted = false
			m.InFlight.DrainStartedAt = time.Time{}
		case session.TurnStarted:
			if m.shouldDrainLateEvent(msg.Timestamp) {
				return m, m.awaitSessionEvent()
			}
			m.InFlight.DrainUntilTurnStarted = false
			m.InFlight.DrainStartedAt = time.Time{}
		default:
			return m, m.awaitSessionEvent()
		}
	}

	switch msg := ev.(type) {
	case session.StatusChanged:
		return m.handleStatusChanged(msg)

	case session.TokenUsage:
		return m.handleTokenUsage(msg)

	case session.TurnStarted:
		return m.handleTurnStarted(msg)

	case session.TurnFinished:
		return m.handleTurnFinished()

	case session.ThinkingDelta:
		return m.handleThinkingDelta(msg)

	case session.UserMessage:
		return m.handleUserMessage(msg)

	case session.AgentDelta:
		return m.handleAgentDelta(msg)

	case session.AgentMessage:
		return m.handleAgentMessage(msg)

	case session.ToolCallStarted:
		return m.handleToolCallStarted(msg)

	case session.ToolOutputDelta:
		return m.handleToolOutputDelta(msg)

	case session.ToolResult:
		return m.handleToolResult(msg)

	case session.VerificationResult:
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		return m.handleChildRequested(msg)

	case session.ChildStarted:
		return m.handleChildStarted(msg)

	case session.ChildDelta:
		return m.handleChildDelta(msg)

	case session.ChildCompleted:
		return m.handleChildCompleted(msg)

	case session.ChildBlocked:
		return m.handleChildBlocked(msg)

	case session.ChildFailed:
		return m.handleChildFailed(msg)

	case session.Error:
		return m.handleSessionError(msg.Err, true)
	}

	return m, m.awaitSessionEvent()
}

func (m Model) handleUserMessage(msg session.UserMessage) (Model, tea.Cmd) {
	if strings.TrimSpace(msg.Message) == "" {
		return m, m.awaitSessionEvent()
	}
	entry := session.Entry{
		Role:      session.User,
		Timestamp: msg.Timestamp,
		Content:   msg.Message,
	}
	return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
}

func (m Model) handleStreamClosed() (Model, tea.Cmd) {
	if !m.InFlight.Thinking {
		return m, nil
	}
	m.clearActiveTurnState(true)
	m.Progress.Compacting = false
	m.Progress.Mode = stateError
	m.Progress.Status = ""
	m.Progress.LastError = "session event stream closed"
	m.recordFinishedTurnSummary()

	entry := session.Entry{
		Role:    session.System,
		Content: "Error: " + m.Progress.LastError,
	}
	var cmds []tea.Cmd
	cmds = append(cmds, m.printEntries(entry))
	if err := m.persistEntry(storage.System{
		Type:    "system",
		Content: entry.Content,
		TS:      now(),
	}); err != nil {
		cmds = append(cmds, persistErrorCmd("persist stream close error", err))
	}
	return m, sequenceCmds(cmds...)
}

func (m Model) handleSessionError(err error, awaitTerminal bool) (Model, tea.Cmd) {
	m.clearActiveTurnState(true)
	m.InFlight.DrainUntilTurnStarted = true
	m.InFlight.DrainStartedAt = time.Now()
	m.Progress.Compacting = false
	m.Progress.Mode = stateError
	m.Progress.Status = ""
	m.Progress.StatusUpdatedAt = time.Time{}
	displayErr := sessionErrorDisplay(err)
	var cmds []tea.Cmd
	if limit, ok := classifyProviderLimitError(err); ok {
		displayErr = limit.display()
		if err := m.persistEntry(m.routingDecision("stop", limit.reason, limit.raw)); err != nil {
			cmds = append(cmds, persistErrorCmd("persist routing stop", err))
		}
	}
	m.Progress.LastError = displayErr
	m.Progress.LastTurnSummary = turnSummary{}
	if !m.Progress.TurnStartedAt.IsZero() {
		m.Progress.LastTurnSummary = turnSummary{
			Elapsed: time.Since(m.Progress.TurnStartedAt),
			Input:   m.Progress.CurrentTurnInput,
			Output:  m.Progress.CurrentTurnOutput,
			Cost:    m.Progress.CurrentTurnCost,
		}
	}
	m.Progress.TurnStartedAt = time.Time{}
	entry := session.Entry{Role: session.System, Content: "Error: " + displayErr}
	printErr := m.printEntries(entry)
	cmds = append([]tea.Cmd{printErr}, cmds...)
	if awaitTerminal {
		if err := m.persistEntry(storage.System{
			Type:    "system",
			Content: entry.Content,
			TS:      now(),
		}); err != nil {
			cmds = append(cmds, persistErrorCmd("persist session error", err))
		}
	}
	if !awaitTerminal {
		return m, sequenceCmds(cmds...)
	}
	cmds = append(cmds, m.awaitSessionEvent())
	return m, sequenceCmds(cmds...)
}

func (m Model) handleLocalError(err error) (Model, tea.Cmd) {
	if !m.InFlight.Thinking {
		m.Progress.Compacting = false
		if isLocalBusyStatus(m.Progress.Status) {
			m.Progress.Status = ""
		}
		if m.Progress.Mode == stateError {
			m.Progress.Mode = stateReady
		}
		m.Progress.LastError = ""
	}
	entry := session.Entry{Role: session.System, Content: "Error: " + err.Error()}
	return m, m.printEntries(entry)
}

func isLocalBusyStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	return trimmed == "Switching runtime..." ||
		trimmed == "Loading session..." ||
		trimmed == "Checking provider..." ||
		trimmed == "Saving provider setup..." ||
		isCompactingStatus(trimmed)
}

func (m Model) handleStatusChanged(msg session.StatusChanged) (Model, tea.Cmd) {
	if msg.AgentID == "" {
		m.Progress.Status = msg.Status
		m.Progress.StatusUpdatedAt = msg.Timestamp
		if m.Progress.StatusUpdatedAt.IsZero() {
			m.Progress.StatusUpdatedAt = time.Now()
		}
		m.Progress.Compacting = isCompactingStatus(msg.Status)
	} else if p, ok := m.InFlight.Subagents[msg.AgentID]; ok {
		p.Status = msg.Status
	}
	if err := m.persistEntry(storage.Status{
		Type:   "status",
		Status: msg.Status,
		TS:     entryUnix(msg.Timestamp),
	}); err != nil {
		return m, m.persistErrorAndAwait("persist status", err)
	}
	return m, m.awaitSessionEvent()
}

func sessionErrorDisplay(err error) string {
	if err == nil {
		return "session error"
	}
	raw := strings.Join(strings.Fields(err.Error()), " ")
	if raw == "" {
		return "session error"
	}
	if strings.Contains(strings.ToLower(raw), "assistant response has no content") {
		return "Provider returned an empty response. Try again or switch models."
	}
	return privacy.Redact(raw)
}

func (m Model) handleTokenUsage(msg session.TokenUsage) (Model, tea.Cmd) {
	m.Progress.TokensSent += msg.Input
	m.Progress.TokensReceived += msg.Output
	m.Progress.ContextTokens += tokenUsageTotal(msg)
	m.Progress.TotalCost += msg.Cost
	m.Progress.CurrentTurnInput += msg.Input
	m.Progress.CurrentTurnOutput += msg.Output
	m.Progress.CurrentTurnCost += msg.Cost
	var cmds []tea.Cmd
	if err := m.persistEntry(storage.TokenUsage{
		Type:   "token_usage",
		Input:  msg.Input,
		Output: msg.Output,
		Cost:   msg.Cost,
		TS:     entryUnix(msg.Timestamp),
	}); err != nil {
		cmds = append(cmds, persistErrorCmd("persist token usage", err))
	}
	if reason := m.configuredBudgetStopReason(); reason != "" &&
		reason != m.Progress.BudgetStopReason {
		m.Progress.BudgetStopReason = reason
		if err := m.persistEntry(m.routingDecision("stop", "budget_limit", reason)); err != nil {
			cmds = append(cmds, persistErrorCmd("persist routing stop", err))
		}
		if m.InFlight.Thinking {
			if err := m.Model.Session.CancelTurn(context.Background()); err != nil {
				cmds = append(cmds, persistErrorCmd("cancel over-budget turn", err))
				cmds = append(cmds, m.awaitSessionEvent())
				return m, sequenceCmds(cmds...)
			}
			m.clearActiveTurnState(true)
			m.InFlight.DrainUntilTurnStarted = true
			m.InFlight.DrainStartedAt = time.Now()
			m.Progress.Mode = stateCancelled
			m.Progress.Status = ""
			entry := session.Entry{
				Role:      session.System,
				Timestamp: msg.Timestamp,
				Content:   "Canceled: " + reason,
			}
			if err := m.persistEntry(storage.System{
				Type:    "system",
				Content: entry.Content,
				TS:      entryUnix(msg.Timestamp),
			}); err != nil {
				cmds = append(cmds, persistErrorCmd("persist budget cancellation", err))
			}
			cmds = append([]tea.Cmd{m.printEntries(entry)}, cmds...)
			cmds = append(cmds, m.awaitSessionEvent())
			return m, sequenceCmds(cmds...)
		}
	}
	cmds = append(cmds, m.awaitSessionEvent())
	return m, sequenceCmds(cmds...)
}

func (m Model) handleTurnStarted(msg session.TurnStarted) (Model, tea.Cmd) {
	m.InFlight.Thinking = true
	m.InFlight.DrainUntilTurnStarted = false
	m.Progress.Compacting = false
	m.Progress.Mode = stateIonizing
	m.Progress.Status = ""
	m.Progress.LastError = ""
	m.Progress.TurnStartedAt = time.Now()
	m.Progress.CurrentTurnInput = 0
	m.Progress.CurrentTurnOutput = 0
	m.Progress.CurrentTurnCost = 0
	m.Progress.ContextTokens = 0
	m.Progress.BudgetStopReason = ""
	m.InFlight.Pending = &session.Entry{Role: session.Agent, Timestamp: msg.Timestamp}
	m.InFlight.PendingTools = nil
	m.InFlight.AgentCommitted = false
	return m, m.awaitSessionEvent()
}

func (m Model) shouldDrainLateEvent(timestamp time.Time) bool {
	if m.InFlight.DrainStartedAt.IsZero() || timestamp.IsZero() {
		return false
	}
	return !timestamp.After(m.InFlight.DrainStartedAt)
}

func (m Model) handleTurnFinished() (Model, tea.Cmd) {
	m.InFlight.Thinking = false
	var cmds []tea.Cmd

	assistantCompleted, printAssistant := m.finishPendingAssistant()
	cmds = appendCmd(cmds, printAssistant)
	cmds = m.finishTurnMode(assistantCompleted, cmds)
	m.recordFinishedTurnSummary()

	if queued := m.popQueuedTurn(); queued != "" {
		cmds = append(cmds, func() tea.Msg {
			return queuedTurnMsg{text: queued, rearmSessionEvents: true}
		})
		return m, tea.Sequence(cmds...)
	}
	cmds = append(cmds, loadGitDiffStats(m.App.Workdir))
	cmds = append(cmds, m.awaitSessionEvent())
	return m, tea.Sequence(cmds...)
}

func (m *Model) finishPendingAssistant() (bool, tea.Cmd) {
	assistantCompleted := m.InFlight.AgentCommitted
	if !m.InFlight.AgentCommitted &&
		m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Agent &&
		(strings.TrimSpace(m.InFlight.Pending.Content) != "" ||
			strings.TrimSpace(m.InFlight.Pending.Reasoning) != "" ||
			strings.TrimSpace(m.InFlight.ReasonBuf) != "") {
		if strings.TrimSpace(m.InFlight.Pending.Reasoning) == "" {
			m.InFlight.Pending.Reasoning = m.InFlight.ReasonBuf
		}
		entry := *m.InFlight.Pending
		m.clearPendingAssistant()
		return true, m.printEntries(entry)
	}
	if m.InFlight.AgentCommitted {
		m.clearPendingAssistant()
	}
	return assistantCompleted, nil
}

func (m *Model) clearPendingAssistant() {
	m.InFlight.Pending = nil
	m.InFlight.StreamBuf = ""
	m.InFlight.ReasonBuf = ""
}

func (m *Model) finishTurnMode(assistantCompleted bool, cmds []tea.Cmd) []tea.Cmd {
	switch {
	case m.Progress.Mode == stateError:
		m.clearActiveTurnState(true)
		m.InFlight.QueuedTurns = nil
		m.Progress.Status = ""
	case m.Progress.Mode == stateCancelled || m.Progress.BudgetStopReason != "":
		m.clearActiveTurnState(true)
		m.Progress.Mode = stateCancelled
		m.InFlight.QueuedTurns = nil
		m.Progress.Status = ""
	case !assistantCompleted:
		m.clearActiveTurnState(true)
		m.Progress.Mode = stateError
		m.Progress.LastError = "turn finished without assistant response"
		m.InFlight.QueuedTurns = nil
		m.Progress.Status = ""
		cmds = append(cmds, m.printEntries(session.Entry{
			Role:    session.System,
			Content: "Error: turn finished without assistant response",
		}))
	default:
		m.Progress.Mode = stateComplete
	}
	return cmds
}

func (m *Model) recordFinishedTurnSummary() {
	if !m.Progress.TurnStartedAt.IsZero() {
		m.Progress.LastTurnSummary = turnSummary{
			Elapsed: time.Since(m.Progress.TurnStartedAt),
			Input:   m.Progress.CurrentTurnInput,
			Output:  m.Progress.CurrentTurnOutput,
			Cost:    m.Progress.CurrentTurnCost,
		}
	}
	m.Progress.TurnStartedAt = time.Time{}
}

func (m *Model) popQueuedTurn() string {
	if len(m.InFlight.QueuedTurns) == 0 {
		return ""
	}
	queued := m.InFlight.QueuedTurns[0]
	m.InFlight.QueuedTurns = m.InFlight.QueuedTurns[1:]
	return queued
}

func appendCmd(cmds []tea.Cmd, cmd tea.Cmd) []tea.Cmd {
	if cmd == nil {
		return cmds
	}
	return append(cmds, cmd)
}

func (m Model) handleThinkingDelta(msg session.ThinkingDelta) (Model, tea.Cmd) {
	if msg.AgentID == "" && m.InFlight.AgentCommitted {
		return m, m.awaitSessionEvent()
	}
	if msg.AgentID == "" {
		m.InFlight.ReasonBuf += msg.Delta
	} else if p, ok := m.InFlight.Subagents[msg.AgentID]; ok {
		p.Reasoning += msg.Delta
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleAgentDelta(msg session.AgentDelta) (Model, tea.Cmd) {
	if msg.AgentID == "" && m.InFlight.AgentCommitted {
		return m, m.awaitSessionEvent()
	}
	if msg.AgentID == "" {
		m.Progress.Mode = stateStreaming
		if m.InFlight.Pending == nil || m.InFlight.Pending.Role != session.Agent {
			m.InFlight.Pending = &session.Entry{
				Role:      session.Agent,
				Timestamp: msg.Timestamp,
			}
		}
		m.InFlight.Pending.Content += msg.Delta
		m.InFlight.StreamBuf = m.InFlight.Pending.Content
	} else if p, ok := m.InFlight.Subagents[msg.AgentID]; ok {
		p.Output += msg.Delta
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleAgentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	if msg.AgentID != "" {
		return m.handleSubagentMessage(msg)
	}
	if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Agent {
		if msg.Message != "" {
			m.InFlight.Pending.Content = msg.Message
		}
		m.InFlight.Pending.Reasoning = m.InFlight.ReasonBuf
		if msg.Reasoning != "" {
			m.InFlight.Pending.Reasoning = msg.Reasoning
		}
		setEntryTimestamp(m.InFlight.Pending, msg.Timestamp)
		entry := *m.InFlight.Pending
		m.InFlight.Pending = nil
		m.InFlight.StreamBuf = ""
		m.InFlight.ReasonBuf = ""
		if strings.TrimSpace(entry.Content) == "" && strings.TrimSpace(entry.Reasoning) == "" {
			return m, m.awaitSessionEvent()
		}

		m.InFlight.AgentCommitted = true
		return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
	}
	entry := session.Entry{
		Role:      session.Agent,
		Timestamp: msg.Timestamp,
		Content:   msg.Message,
		Reasoning: m.InFlight.ReasonBuf,
	}
	if msg.Reasoning != "" {
		entry.Reasoning = msg.Reasoning
	}
	m.InFlight.StreamBuf = ""
	m.InFlight.ReasonBuf = ""
	if strings.TrimSpace(entry.Content) == "" && strings.TrimSpace(entry.Reasoning) == "" {
		return m, m.awaitSessionEvent()
	}
	m.InFlight.AgentCommitted = true
	return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
}

func (m Model) handleToolCallStarted(msg session.ToolCallStarted) (Model, tea.Cmd) {
	m.Progress.Mode = stateWorking
	m.Progress.LastToolUseID = msg.ToolUseID
	if m.Progress.LastToolUseID == "" {
		m.Progress.LastToolUseID = session.ShortID()
	}
	entry := &session.Entry{
		Role:      session.Tool,
		Timestamp: msg.Timestamp,
		Title:     privacy.Redact(m.formatToolTitle(msg.ToolName, msg.Args)),
	}
	if m.InFlight.PendingTools == nil {
		m.InFlight.PendingTools = make(map[string]*session.Entry)
	}
	m.InFlight.PendingTools[m.Progress.LastToolUseID] = entry
	if m.InFlight.Pending == nil || m.InFlight.Pending.Role == session.Tool ||
		(m.InFlight.Pending.Role == session.Agent &&
			m.InFlight.Pending.Content == "" &&
			m.InFlight.ReasonBuf == "") {
		m.InFlight.Pending = entry
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolOutputDelta(msg session.ToolOutputDelta) (Model, tea.Cmd) {
	if entry := m.pendingToolEntry(msg.ToolUseID); entry != nil {
		entry.Content += msg.Delta
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolResult(msg session.ToolResult) (Model, tea.Cmd) {
	toolUseID := msg.ToolUseID
	if toolUseID == "" {
		toolUseID = m.Progress.LastToolUseID
	}
	if pending := m.pendingToolEntry(toolUseID); pending != nil {
		pending.Content = msg.Result
		pending.IsError = msg.Error != nil
		setEntryTimestamp(pending, msg.Timestamp)
		entry := *pending
		m.clearPendingTool(toolUseID, pending)
		if len(m.InFlight.PendingTools) == 0 {
			m.Progress.Mode = stateIonizing
			m.Progress.Status = ""
			m.Progress.ContextTokens = 0
		}

		return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
	}
	return m, m.awaitSessionEvent()
}

func tokenUsageTotal(msg session.TokenUsage) int {
	if msg.Total > 0 {
		return msg.Total
	}
	return msg.Input + msg.Output
}

func (m Model) pendingToolEntry(toolUseID string) *session.Entry {
	if toolUseID != "" {
		return m.InFlight.PendingTools[toolUseID]
	}
	if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Tool {
		return m.InFlight.Pending
	}
	return nil
}

func (m *Model) clearPendingTool(toolUseID string, entry *session.Entry) {
	if toolUseID != "" {
		delete(m.InFlight.PendingTools, toolUseID)
	}
	if len(m.InFlight.PendingTools) == 0 {
		m.InFlight.PendingTools = nil
	}
	if m.InFlight.Pending == entry {
		m.InFlight.Pending = nil
		for _, id := range sortedKeys(m.InFlight.PendingTools) {
			m.InFlight.Pending = m.InFlight.PendingTools[id]
			break
		}
	}
}
