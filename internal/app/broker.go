package app

import (
	"context"
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// Broker handles the communication between the Ion TUI and the backend.
// It translates backend events into Ion TUI messages.
type Broker struct{}

func (m Model) awaitSessionEvent() tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-m.session.Events()
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
		m.status = msg.Status
		if err := m.persistEntry("persist status", storage.Status{
			Type:   "status",
			Status: msg.Status,
			TS:     now(),
		}); err != nil {
			return m, persistErrorCmd("persist status", err)
		}
		return m, m.awaitSessionEvent()

	case session.TokenUsage:
		m.tokensSent += msg.Input
		m.tokensReceived += msg.Output
		m.totalCost += msg.Cost
		m.currentTurnInput += msg.Input
		m.currentTurnOutput += msg.Output
		m.currentTurnCost += msg.Cost
		if err := m.persistEntry("persist token usage", storage.TokenUsage{
			Type:   "token_usage",
			Input:  msg.Input,
			Output: msg.Output,
			Cost:   msg.Cost,
			TS:     now(),
		}); err != nil {
			return m, persistErrorCmd("persist token usage", err)
		}
		return m, m.awaitSessionEvent()

	case session.TurnStarted:
		m.thinking = true
		m.progress = stateIonizing
		m.turnStartedAt = time.Now()
		m.currentTurnInput = 0
		m.currentTurnOutput = 0
		m.currentTurnCost = 0
		m.pending = &session.Entry{Role: session.Agent}
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		m.thinking = false
		m.progress = stateComplete
		if !m.turnStartedAt.IsZero() {
			m.lastTurnSummary = turnSummary{
				Elapsed: time.Since(m.turnStartedAt),
				Input:   m.currentTurnInput,
				Output:  m.currentTurnOutput,
				Cost:    m.currentTurnCost,
			}
		}
		m.turnStartedAt = time.Time{}
		if queued := strings.TrimSpace(m.queuedTurn); queued != "" {
			m.queuedTurn = ""
			return m, func() tea.Msg { return queuedTurnMsg{text: queued} }
		}
		return m, m.awaitSessionEvent()

	case session.ThinkingDelta:
		m.reasonBuf += msg.Delta
		return m, m.awaitSessionEvent()

	case session.AgentDelta:
		m.progress = stateStreaming
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.Agent}
		}
		m.pending.Content += msg.Delta
		m.streamBuf = m.pending.Content
		return m, m.awaitSessionEvent()

	case session.AgentMessage:
		if m.pending != nil && m.pending.Role == session.Agent {
			if msg.Message != "" {
				m.pending.Content = msg.Message
			}
			m.pending.Reasoning = m.reasonBuf
			entry := *m.pending
			m.pending = nil
			m.streamBuf = ""
			m.reasonBuf = ""
			if strings.TrimSpace(entry.Content) == "" && strings.TrimSpace(entry.Reasoning) == "" {
				return m, m.awaitSessionEvent()
			}

			blocks := []storage.Block{}
			if entry.Reasoning != "" {
				blocks = append(blocks, storage.Block{
					Type:     "thinking",
					Thinking: &entry.Reasoning,
				})
			}
			blocks = append(blocks, storage.Block{
				Type: "text",
				Text: &entry.Content,
			})
			if err := m.persistEntry("persist agent response", storage.Agent{
				Type:    "agent",
				Content: blocks,
				TS:      now(),
			}); err != nil {
				return m, tea.Sequence(m.printEntries(entry), persistErrorCmd("persist agent response", err))
			}
			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.ToolCallStarted:
		m.progress = stateWorking
		m.lastToolUseID = session.ShortID()
		m.pending = &session.Entry{
			Role:  session.Tool,
			Title: FormatToolTitle(msg.ToolName, msg.Args),
		}
		if err := m.persistEntry("persist tool use", storage.ToolUse{
			Type: "tool_use",
			ID:   m.lastToolUseID,
			Name: msg.ToolName,
			Input: map[string]string{
				"args": msg.Args,
			},
			TS: now(),
		}); err != nil {
			return m, persistErrorCmd("persist tool use", err)
		}
		return m, m.awaitSessionEvent()

	case session.ToolOutputDelta:
		if m.pending != nil && m.pending.Role == session.Tool {
			m.pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ToolResult:
		if m.pending != nil && m.pending.Role == session.Tool {
			m.pending.Content = msg.Result
			m.pending.IsError = msg.Error != nil
			entry := *m.pending
			m.pending = nil

			if err := m.persistEntry("persist tool result", storage.ToolResult{
				Type:      "tool_result",
				ToolUseID: m.lastToolUseID,
				Content:   msg.Result,
				IsError:   msg.Error != nil,
				TS:        now(),
			}); err != nil {
				return m, tea.Sequence(m.printEntries(entry), persistErrorCmd("persist tool result", err))
			}
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
		if err := m.persistEntry("persist verification result", storage.ToolResult{
			Type:      "tool_result",
			ToolUseID: m.lastToolUseID,
			Content:   content,
			IsError:   !msg.Passed,
			TS:        now(),
		}); err != nil {
			return m, tea.Sequence(m.printEntries(entry), persistErrorCmd("persist verification result", err))
		}
		return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())

	case session.ApprovalRequest:
		m.pendingApproval = &msg
		m.progress = stateApproval
		m.thinking = false
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		m.pending = &session.Entry{
			Role:    session.Subagent,
			Title:   msg.AgentName,
			Content: msg.Query,
		}
		return m, m.awaitSessionEvent()

	case session.ChildStarted:
		if m.pending != nil && m.pending.Role == session.Subagent {
			m.pending.Title = msg.AgentName
		}
		return m, m.awaitSessionEvent()

	case session.ChildDelta:
		if m.pending != nil && m.pending.Role == session.Subagent {
			m.pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ChildCompleted:
		if m.pending != nil && m.pending.Role == session.Subagent {
			m.pending.Content = msg.Result
			entry := *m.pending
			m.pending = nil
			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.ChildFailed:
		if m.pending != nil && m.pending.Role == session.Subagent {
			m.pending.Content = "ERROR: " + msg.Error
			m.pending.IsError = true
			entry := *m.pending
			m.pending = nil
			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.Error:
		m.pending = nil
		m.pendingApproval = nil
		m.streamBuf = ""
		m.reasonBuf = ""
		m.thinking = false
		m.progress = stateError
		m.lastError = msg.Err.Error()
		if !m.turnStartedAt.IsZero() {
			m.lastTurnSummary = turnSummary{
				Elapsed: time.Since(m.turnStartedAt),
				Input:   m.currentTurnInput,
				Output:  m.currentTurnOutput,
				Cost:    m.currentTurnCost,
			}
		}
		m.turnStartedAt = time.Time{}
		entry := session.Entry{Role: session.System, Content: "Error: " + msg.Err.Error()}
		return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
	}

	return m, m.awaitSessionEvent()
}

func persistErrorCmd(action string, err error) tea.Cmd {
	return func() tea.Msg {
		return session.Error{Err: fmt.Errorf("%s: %w", action, err)}
	}
}

func (m Model) persistEntry(action string, entry any) error {
	if m.storage == nil {
		return nil
	}
	if err := m.storage.Append(context.Background(), entry); err != nil {
		return fmt.Errorf("%s: %w", action, err)
	}
	return nil
}

func (m Model) submitText(text string) (Model, tea.Cmd) {
	m.history = append(m.history, text)
	m.historyIdx = -1
	m.historyDraft = ""

	userEntry := session.Entry{Role: session.User, Content: text}
	m.composer.Reset()
	m.relayoutComposer()

	if err := m.persistEntry("persist user input", storage.User{
		Type:    "user",
		Content: text,
		TS:      now(),
	}); err != nil {
		return m, persistErrorCmd("persist user input", err)
	}

	if strings.HasPrefix(text, "/") {
		m, cmd := m.handleCommand(text)
		return m, tea.Sequence(m.printEntries(userEntry), cmd)
	}

	m.progress = stateIonizing
	m.thinking = true
	m.session.SubmitTurn(context.Background(), text)
	return m, m.printEntries(userEntry)
}
