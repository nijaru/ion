package app

import (
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

	case session.ChildCanceled:
		return m.handleChildCanceled(msg)

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
	m.turnReducer().clearActiveState(true)
	m.Progress.Compacting = false
	m.Progress.Mode = stateError
	m.Progress.Status = ""
	m.Progress.LastError = "session event stream closed"
	m.turnReducer().recordFinishedTurnSummary(time.Now())

	entry := session.Entry{
		Role:    session.System,
		Content: "Error: " + m.Progress.LastError,
	}
	var cmds []tea.Cmd
	cmds = append(cmds, m.printEntries(entry))
	cmds = append(cmds, m.persistEntryCmd("persist stream close error", storage.System{
		Type:    "system",
		Content: entry.Content,
		TS:      now(),
	}))
	return m, sequenceCmds(cmds...)
}

func (m Model) handleSessionError(err error, awaitTerminal bool) (Model, tea.Cmd) {
	m.turnReducer().clearActiveState(true)
	m.turnReducer().beginDrain(time.Now())
	m.Progress.Compacting = false
	m.Progress.Mode = stateError
	m.Progress.Status = ""
	m.Progress.StatusUpdatedAt = time.Time{}
	displayErr := sessionErrorDisplay(err)
	var cmds []tea.Cmd
	if limit, ok := classifyProviderLimitError(err); ok {
		displayErr = limit.display()
		cmds = append(
			cmds,
			m.persistEntryCmd(
				"persist routing stop",
				m.routingDecision("stop", limit.reason, limit.raw),
			),
		)
	}
	m.Progress.LastError = displayErr
	m.Progress.LastTurnSummary = turnSummary{}
	m.turnReducer().recordFinishedTurnSummary(time.Now())
	entry := session.Entry{Role: session.System, Content: "Error: " + displayErr}
	printErr := m.printEntries(entry)
	cmds = append([]tea.Cmd{printErr}, cmds...)
	if awaitTerminal {
		cmds = append(cmds, m.persistEntryCmd("persist session error", storage.System{
			Type:    "system",
			Content: entry.Content,
			TS:      now(),
		}))
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
		trimmed == "Saving runtime settings..." ||
		trimmed == "Loading session..." ||
		trimmed == "Checking provider..." ||
		trimmed == "Saving provider setup..." ||
		trimmed == "Loading settings..." ||
		trimmed == "Saving settings..." ||
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
	return m, sequenceCmds(m.persistEntryCmd("persist status", storage.Status{
		Type:   "status",
		Status: msg.Status,
		TS:     entryUnix(msg.Timestamp),
	}), m.awaitSessionEvent())
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
	m.turnReducer().applyTokenUsage(msg)
	cmds := []tea.Cmd{m.persistEntryCmd("persist token usage", storage.TokenUsage{
		Type:   "token_usage",
		Input:  msg.Input,
		Output: msg.Output,
		Cost:   msg.Cost,
		TS:     entryUnix(msg.Timestamp),
	})}
	if reason := m.configuredBudgetStopReason(); reason != "" &&
		reason != m.Progress.BudgetStopReason {
		m.Progress.BudgetStopReason = reason
		cmds = append(
			cmds,
			m.persistEntryCmd(
				"persist routing stop",
				m.routingDecision("stop", "budget_limit", reason),
			),
		)
		if m.InFlight.Thinking {
			m.turnReducer().clearActiveState(true)
			m.turnReducer().beginDrain(time.Now())
			m.Progress.Mode = stateCancelled
			m.Progress.Status = ""
			entry := session.Entry{
				Role:      session.System,
				Timestamp: msg.Timestamp,
				Content:   "Canceled: " + reason,
			}
			cmds = append(cmds, m.persistEntryCmd("persist budget cancellation", storage.System{
				Type:    "system",
				Content: entry.Content,
				TS:      entryUnix(msg.Timestamp),
			}))
			cmds = append([]tea.Cmd{
				tea.Batch(
					m.printEntries(entry),
					cancelTurnCmd(m.Model.Session),
				),
			}, cmds...)
			cmds = append(cmds, m.awaitSessionEvent())
			return m, sequenceCmds(cmds...)
		}
	}
	cmds = append(cmds, m.awaitSessionEvent())
	return m, sequenceCmds(cmds...)
}

func (m Model) handleTurnStarted(msg session.TurnStarted) (Model, tea.Cmd) {
	m.turnReducer().startTurn(msg.Timestamp, time.Now())
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

	assistant, assistantCompleted, printAssistant := m.turnReducer().finishPendingAssistant()
	if printAssistant {
		cmds = append(cmds, m.printEntries(assistant))
	}
	if entry, ok := m.turnReducer().finishTurnMode(assistantCompleted); ok {
		cmds = append(cmds, m.printEntries(entry))
	}
	m.turnReducer().recordFinishedTurnSummary(time.Now())

	if queued := m.turnReducer().popQueuedTurn(); queued != "" {
		cmds = append(cmds, func() tea.Msg {
			return queuedTurnMsg{text: queued, rearmSessionEvents: true}
		})
		return m, tea.Sequence(cmds...)
	}
	cmds = append(cmds, loadGitDiffStats(m.App.Workdir))
	cmds = append(cmds, m.awaitSessionEvent())
	return m, tea.Sequence(cmds...)
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
	m.turnReducer().appendToolOutput(msg.ToolUseID, msg.Delta)
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolResult(msg session.ToolResult) (Model, tea.Cmd) {
	toolUseID := msg.ToolUseID
	if toolUseID == "" {
		toolUseID = m.Progress.LastToolUseID
	}
	if entry, ok := m.turnReducer().completeToolResult(toolUseID, msg); ok {
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
