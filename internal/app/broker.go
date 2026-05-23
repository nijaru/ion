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
	turn := m.turnReducer()
	if turn.drainingUntilTurnStarted() {
		switch msg := ev.(type) {
		case session.UserMessage:
			if turn.shouldDrainLateEvent(msg.Timestamp) {
				return m, m.awaitSessionEvent()
			}
			turn.finishDrain()
		case session.TurnStarted:
			if turn.shouldDrainLateEvent(msg.Timestamp) {
				return m, m.awaitSessionEvent()
			}
			turn.finishDrain()
		case session.TurnFinished:
			turn.finishDrain()
		default:
			return m, m.awaitSessionEvent()
		}
	}

	switch msg := ev.(type) {
	case session.StatusChanged:
		return m.handleStatusChanged(msg)

	case session.TokenUsage:
		return m.handleTokenUsage(msg)

	case session.QueuedInputUpdated:
		return m.handleQueuedInputUpdated(msg)

	case session.TurnStarted:
		return m.handleTurnStarted(msg)

	case session.TurnSavePoint:
		return m, m.awaitSessionEvent()

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
	entry, ok := m.turnReducer().streamClosed(time.Now())
	if !ok {
		return m, nil
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
	m.turnReducer().failTurn(displayErr, time.Now())
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
	m.turnReducer().clearLocalErrorIfIdle()
	if !m.InFlight.Thinking {
		m.progressReducer().clearLocalBusyStatus()
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
	m.turnReducer().applyStatusChanged(msg)
	return m, sequenceCmds(m.persistEntryCmd("persist status", storage.Status{
		Type:   "status",
		Status: msg.Status,
		TS:     entryUnix(msg.Timestamp),
	}), m.awaitSessionEvent())
}

func (m Model) handleQueuedInputUpdated(msg session.QueuedInputUpdated) (Model, tea.Cmd) {
	m.turnReducer().setBackendQueuedTurns(msg.Snapshot.FollowUp)
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
		entry, _ := m.turnReducer().applyBudgetStop(reason, msg.Timestamp)
		cmds = append(
			cmds,
			m.persistEntryCmd(
				"persist routing stop",
				m.routingDecision("stop", "budget_limit", reason),
			),
		)
		if entry.Content != "" {
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

func (m Model) handleTurnFinished() (Model, tea.Cmd) {
	m.turnReducer().stopThinking()
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
	m.turnReducer().appendThinkingDelta(msg.AgentID, msg.Delta)
	return m, m.awaitSessionEvent()
}

func (m Model) handleAgentDelta(msg session.AgentDelta) (Model, tea.Cmd) {
	m.turnReducer().appendAgentDelta(msg.AgentID, msg.Delta, msg.Timestamp)
	return m, m.awaitSessionEvent()
}

func (m Model) handleAgentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	if msg.AgentID != "" {
		return m.handleSubagentMessage(msg)
	}
	if entry, ok := m.turnReducer().commitAgentMessage(msg); ok {
		return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolCallStarted(msg session.ToolCallStarted) (Model, tea.Cmd) {
	m.turnReducer().startToolCall(
		msg.ToolUseID,
		msg.Timestamp,
		privacy.Redact(m.formatToolTitle(msg.ToolName, msg.Args)),
	)
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
