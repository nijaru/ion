package app

import (
	"context"
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/privacy"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// Broker handles the communication between the Ion TUI and the backend.
// It translates backend events into Ion TUI messages.
type Broker struct{}

type localErrorMsg struct {
	err error
}

func (m Model) awaitSessionEvent() tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-m.Model.Session.Events()
		if !ok {
			return streamClosedMsg{}
		}
		return ev
	}
}

// handleSessionEvent processes events from the agent session channel.
func (m Model) handleSessionEvent(ev session.Event) (Model, tea.Cmd) {
	switch msg := ev.(type) {
	case session.StatusChanged:
		if msg.AgentID == "" {
			m.Progress.Status = msg.Status
			m.Progress.Compacting = isCompactingStatus(msg.Status)
		} else {
			if p, ok := m.InFlight.Subagents[msg.AgentID]; ok {
				p.Status = msg.Status
			}
		}
		if err := m.persistEntry("persist status", storage.Status{
			Type:   "status",
			Status: msg.Status,
			TS:     now(),
		}); err != nil {
			return m, persistErrorCmd("persist status", err)
		}
		return m, m.awaitSessionEvent()

	case session.TokenUsage:
		m.Progress.TokensSent += msg.Input
		m.Progress.TokensReceived += msg.Output
		m.Progress.TotalCost += msg.Cost
		m.Progress.CurrentTurnInput += msg.Input
		m.Progress.CurrentTurnOutput += msg.Output
		m.Progress.CurrentTurnCost += msg.Cost
		if err := m.persistEntry("persist token usage", storage.TokenUsage{
			Type:   "token_usage",
			Input:  msg.Input,
			Output: msg.Output,
			Cost:   msg.Cost,
			TS:     now(),
		}); err != nil {
			return m, persistErrorCmd("persist token usage", err)
		}
		if reason := m.configuredBudgetStopReason(); reason != "" && reason != m.Progress.BudgetStopReason {
			m.Progress.BudgetStopReason = reason
			if err := m.persistEntry("persist routing stop", m.routingDecision("stop", "budget_limit", reason)); err != nil {
				return m, persistErrorCmd("persist routing stop", err)
			}
			if m.InFlight.Thinking {
				if err := m.Model.Session.CancelTurn(context.Background()); err != nil {
					return m, persistErrorCmd("cancel over-budget turn", err)
				}
				m.InFlight.Thinking = false
				m.Progress.Mode = stateCancelled
				entry := session.Entry{Role: session.System, Content: "Canceled: " + reason}
				return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
			}
		}
		return m, m.awaitSessionEvent()

	case session.TurnStarted:
		m.InFlight.Thinking = true
		m.Progress.Compacting = false
		m.Progress.Mode = stateIonizing
		m.Progress.LastError = ""
		m.Progress.TurnStartedAt = time.Now()
		m.Progress.CurrentTurnInput = 0
		m.Progress.CurrentTurnOutput = 0
		m.Progress.CurrentTurnCost = 0
		m.Progress.BudgetStopReason = ""
		m.InFlight.Pending = &session.Entry{Role: session.Agent}
		m.InFlight.PendingTools = nil
		m.InFlight.AgentCommitted = false
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		m.InFlight.Thinking = false
		var cmds []tea.Cmd
		if !m.InFlight.AgentCommitted &&
			m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Agent &&
			(strings.TrimSpace(m.InFlight.Pending.Content) != "" ||
				strings.TrimSpace(m.InFlight.Pending.Reasoning) != "" ||
				strings.TrimSpace(m.InFlight.ReasonBuf) != "") {
			if strings.TrimSpace(m.InFlight.Pending.Reasoning) == "" {
				m.InFlight.Pending.Reasoning = m.InFlight.ReasonBuf
			}
			entry := *m.InFlight.Pending
			m.InFlight.Pending = nil
			m.InFlight.StreamBuf = ""
			m.InFlight.ReasonBuf = ""
			cmds = append(cmds, m.printEntries(entry))
		}
		if m.InFlight.AgentCommitted {
			m.InFlight.Pending = nil
			m.InFlight.StreamBuf = ""
			m.InFlight.ReasonBuf = ""
		}
		if m.Progress.Mode == stateError {
			m.InFlight.QueuedTurns = nil
		} else if m.Progress.Mode == stateCancelled || m.Progress.BudgetStopReason != "" {
			m.Progress.Mode = stateCancelled
			m.InFlight.QueuedTurns = nil
		} else {
			m.Progress.Mode = stateComplete
		}
		if !m.Progress.TurnStartedAt.IsZero() {
			m.Progress.LastTurnSummary = turnSummary{
				Elapsed: time.Since(m.Progress.TurnStartedAt),
				Input:   m.Progress.CurrentTurnInput,
				Output:  m.Progress.CurrentTurnOutput,
				Cost:    m.Progress.CurrentTurnCost,
			}
		}
		m.Progress.TurnStartedAt = time.Time{}
		if len(m.InFlight.QueuedTurns) > 0 {
			queued := m.InFlight.QueuedTurns[0]
			m.InFlight.QueuedTurns = m.InFlight.QueuedTurns[1:]
			cmds = append(cmds, func() tea.Msg { return queuedTurnMsg{text: queued} })
			return m, tea.Sequence(cmds...)
		}
		cmds = append(cmds, m.awaitSessionEvent())
		return m, tea.Sequence(cmds...)

	case session.ThinkingDelta:
		if msg.AgentID == "" && m.InFlight.AgentCommitted {
			return m, m.awaitSessionEvent()
		}
		if msg.AgentID == "" {
			m.InFlight.ReasonBuf += msg.Delta
		} else {
			if p, ok := m.InFlight.Subagents[msg.AgentID]; ok {
				p.Reasoning += msg.Delta
			}
		}
		return m, m.awaitSessionEvent()

	case session.AgentDelta:
		if msg.AgentID == "" && m.InFlight.AgentCommitted {
			return m, m.awaitSessionEvent()
		}
		if msg.AgentID == "" {
			m.Progress.Mode = stateStreaming
			if m.InFlight.Pending == nil {
				m.InFlight.Pending = &session.Entry{Role: session.Agent}
			}
			m.InFlight.Pending.Content += msg.Delta
			m.InFlight.StreamBuf = m.InFlight.Pending.Content
		} else {
			if p, ok := m.InFlight.Subagents[msg.AgentID]; ok {
				p.Output += msg.Delta
			}
		}
		return m, m.awaitSessionEvent()

	case session.AgentMessage:
		if msg.AgentID == "" {
			if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Agent {
				if msg.Message != "" {
					m.InFlight.Pending.Content = msg.Message
				}
				m.InFlight.Pending.Reasoning = m.InFlight.ReasonBuf
				if msg.Reasoning != "" {
					m.InFlight.Pending.Reasoning = msg.Reasoning
				}
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
		} else {
			if p, ok := m.InFlight.Subagents[msg.AgentID]; ok {
				content := p.Output
				if msg.Message != "" {
					content = msg.Message
				}
				committed := session.Entry{
					Role:      session.Subagent,
					Title:     p.Name,
					Content:   "Completed: " + content,
					Reasoning: p.Reasoning,
				}
				delete(m.InFlight.Subagents, msg.AgentID)
				return m, tea.Sequence(m.printEntries(committed), m.awaitSessionEvent())
			}
		}
		return m, m.awaitSessionEvent()

	case session.ToolCallStarted:
		redactedArgs := privacy.Redact(msg.Args)
		m.Progress.Mode = stateWorking
		m.Progress.LastToolUseID = msg.ToolUseID
		if m.Progress.LastToolUseID == "" {
			m.Progress.LastToolUseID = session.ShortID()
		}
		entry := &session.Entry{
			Role:  session.Tool,
			Title: FormatToolTitle(msg.ToolName, redactedArgs),
		}
		if m.InFlight.PendingTools == nil {
			m.InFlight.PendingTools = make(map[string]*session.Entry)
		}
		m.InFlight.PendingTools[m.Progress.LastToolUseID] = entry
		if m.InFlight.Pending == nil || m.InFlight.Pending.Role == session.Tool ||
			(m.InFlight.Pending.Role == session.Agent && m.InFlight.Pending.Content == "" && m.InFlight.ReasonBuf == "") {
			m.InFlight.Pending = entry
		}
		return m, m.awaitSessionEvent()

	case session.ToolOutputDelta:
		if entry := m.pendingToolEntry(msg.ToolUseID); entry != nil {
			entry.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ToolResult:
		toolUseID := msg.ToolUseID
		if toolUseID == "" {
			toolUseID = m.Progress.LastToolUseID
		}
		if pending := m.pendingToolEntry(toolUseID); pending != nil {
			pending.Content = msg.Result
			pending.IsError = msg.Error != nil
			entry := *pending
			m.clearPendingTool(toolUseID, pending)

			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.VerificationResult:
		status := ifthen(msg.Passed, "PASSED", "FAILED")
		content := fmt.Sprintf("%s: %s\n%s", status, msg.Metric, msg.Output)
		entry := session.Entry{
			Role:    session.Tool,
			Title:   "verify: " + msg.Command,
			Content: content,
			IsError: !msg.Passed,
		}
		return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())

	case session.ApprovalRequest:
		msg = redactApprovalRequest(msg)
		m.Approval.Pending = &msg
		m.Progress.Mode = stateApproval
		m.InFlight.Thinking = false
		if notify := m.approvalNotificationCmd(msg); notify != nil {
			return m, tea.Batch(notify, m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		p := &SubagentProgress{
			ID:     msg.AgentName,
			Name:   msg.AgentName,
			Intent: msg.Query,
			Status: "Requested",
		}
		if m.InFlight.Subagents == nil {
			m.InFlight.Subagents = make(map[string]*SubagentProgress)
		}
		m.InFlight.Subagents[msg.AgentName] = p
		m.Progress.Mode = stateWorking

		// Persist start breadcrumb
		if err := m.persistEntry("persist subagent start", storage.Subagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: "Started: " + msg.Query,
			IsError: false,
			TS:      now(),
		}); err != nil {
			return m, tea.Sequence(m.printEntries(session.Entry{
				Role:    session.Subagent,
				Title:   p.Name,
				Content: "Started: " + p.Intent,
			}), persistErrorCmd("persist subagent start", err))
		}
		// We print the started entry immediately to scrollback
		return m, tea.Sequence(m.printEntries(session.Entry{
			Role:    session.Subagent,
			Title:   p.Name,
			Content: "Started: " + p.Intent,
		}), m.awaitSessionEvent())

	case session.ChildStarted:
		if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
			p.Status = "Started"
			m.Progress.Mode = stateWorking
		}
		return m, m.awaitSessionEvent()

	case session.ChildDelta:
		if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
			p.Output += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ChildCompleted:
		if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
			p.Status = "Completed"
			p.Output = msg.Result
			committed := session.Entry{
				Role:    session.Subagent,
				Title:   p.Name,
				Content: "Completed: " + p.Output,
			}
			delete(m.InFlight.Subagents, msg.AgentName)
			m.Progress.Mode = stateComplete

			if err := m.persistEntry("persist subagent completion", storage.Subagent{
				Type:    "subagent",
				Name:    msg.AgentName,
				Content: committed.Content,
				IsError: false,
				TS:      now(),
			}); err != nil {
				return m, tea.Sequence(m.printEntries(committed), persistErrorCmd("persist subagent completion", err))
			}
			return m, tea.Sequence(m.printEntries(committed), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.ChildBlocked:
		if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
			p.Status = "Blocked"
			p.Output = "BLOCKED: " + msg.Reason
			// Note: We don't remove from Subagents on block, as it's still active just waiting
			m.Progress.Mode = stateBlocked
			m.InFlight.Thinking = false
			// We keep it in Plane B only, no durable transcript entry yet.
			return m, m.awaitSessionEvent()
		}
		return m, m.awaitSessionEvent()

	case session.ChildFailed:
		if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
			p.Status = "Failed"
			p.Output = "ERROR: " + msg.Error
			committed := session.Entry{
				Role:    session.Subagent,
				Title:   p.Name,
				Content: "Failed: " + msg.Error,
				IsError: true,
			}
			delete(m.InFlight.Subagents, msg.AgentName)
			m.Progress.Mode = stateError
			m.Progress.LastError = "Subagent failed: " + msg.Error

			if err := m.persistEntry("persist subagent failure", storage.Subagent{
				Type:    "subagent",
				Name:    msg.AgentName,
				Content: committed.Content,
				IsError: true,
				TS:      now(),
			}); err != nil {
				return m, tea.Sequence(m.printEntries(committed), persistErrorCmd("persist subagent failure", err))
			}
			return m, tea.Sequence(m.printEntries(committed), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.Error:
		return m.handleSessionError(msg.Err, true)
	}

	return m, m.awaitSessionEvent()
}

func (m Model) handleSessionError(err error, awaitTerminal bool) (Model, tea.Cmd) {
	m.InFlight.Pending = nil
	m.InFlight.PendingTools = nil
	m.Approval.Pending = nil
	m.InFlight.QueuedTurns = nil
	m.InFlight.StreamBuf = ""
	m.InFlight.ReasonBuf = ""
	m.InFlight.Thinking = false
	m.InFlight.AgentCommitted = false
	m.Progress.Compacting = false
	m.Progress.Mode = stateError
	displayErr := err.Error()
	if limit, ok := classifyProviderLimitError(err); ok {
		displayErr = limit.display()
		if err := m.persistEntry("persist routing stop", m.routingDecision("stop", limit.reason, limit.raw)); err != nil {
			return m, persistErrorCmd("persist routing stop", err)
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
	if awaitTerminal {
		if err := m.persistEntry("persist session error", storage.System{
			Type:    "system",
			Content: entry.Content,
			TS:      now(),
		}); err != nil {
			return m, persistErrorCmd("persist session error", err)
		}
	}
	printErr := m.printEntries(entry)
	if !awaitTerminal {
		return m, printErr
	}
	return m, tea.Sequence(printErr, m.awaitSessionEvent())
}

func redactApprovalRequest(req session.ApprovalRequest) session.ApprovalRequest {
	req.Description = privacy.Redact(req.Description)
	req.Args = privacy.Redact(req.Args)
	return req
}

func persistErrorCmd(action string, err error) tea.Cmd {
	return func() tea.Msg {
		return localErrorMsg{err: fmt.Errorf("%s: %w", action, err)}
	}
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

func (m Model) cancelRunningTurn(reason string) (Model, tea.Cmd) {
	if err := m.Model.Session.CancelTurn(context.Background()); err != nil {
		return m, persistErrorCmd("cancel turn", err)
	}
	m.InFlight.Thinking = false
	m.Progress.Mode = stateCancelled
	m.InFlight.Pending = nil
	m.InFlight.PendingTools = nil
	m.InFlight.QueuedTurns = nil
	m.InFlight.StreamBuf = ""
	m.InFlight.ReasonBuf = ""
	m.InFlight.AgentCommitted = false
	entry := session.Entry{Role: session.System, Content: reason}
	if err := m.persistEntry("persist cancellation", storage.System{
		Type:    "system",
		Content: entry.Content,
		TS:      now(),
	}); err != nil {
		return m, persistErrorCmd("persist cancellation", err)
	}
	return m, m.printEntries(entry)
}

func (m Model) persistEntry(action string, entry any) error {
	if m.Model.Storage == nil {
		return nil
	}
	if err := m.Model.Storage.Append(context.Background(), entry); err != nil {
		return fmt.Errorf("%s: %w", action, err)
	}
	return nil
}

func (m Model) submitText(text string) (Model, tea.Cmd) {
	// Expand any paste marker placeholders to their original content.
	text = m.expandMarkers(text)
	m.PasteMarkers = make(map[string]pasteMarker)

	if !strings.HasPrefix(text, "/") {
		if reason := m.configuredSessionBudgetStopReason(); reason != "" {
			return m, cmdError(reason)
		}
	}

	m.Input.History = append(m.Input.History, text)
	m.Input.HistoryIdx = -1
	m.Input.HistoryDraft = ""

	userEntry := session.Entry{Role: session.User, Content: text}
	m.Input.Composer.Reset()
	m.relayoutComposer()

	if strings.HasPrefix(text, "/") {
		m, cmd := m.handleCommand(text)
		return m, tea.Sequence(m.printEntries(userEntry), cmd)
	}

	m.Progress.Mode = stateIonizing
	m.Progress.LastError = ""
	m.InFlight.Thinking = true
	if err := m.Model.Session.SubmitTurn(context.Background(), text); err != nil {
		m, errCmd := m.handleSessionError(err, false)
		return m, tea.Sequence(m.printEntries(userEntry), errCmd)
	}
	if err := m.persistEntry("persist routing decision", m.routingDecision("use_model", "active_preset", "")); err != nil {
		return m, persistErrorCmd("persist routing decision", err)
	}
	return m, m.printEntries(userEntry)
}
