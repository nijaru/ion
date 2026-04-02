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
		m.Progress.Status = msg.Status
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
		return m, m.awaitSessionEvent()

	case session.TurnStarted:
		m.InFlight.Thinking = true
		m.Progress.Mode = stateIonizing
		m.Progress.LastError = ""
		m.Progress.TurnStartedAt = time.Now()
		m.Progress.CurrentTurnInput = 0
		m.Progress.CurrentTurnOutput = 0
		m.Progress.CurrentTurnCost = 0
		m.InFlight.Pending = &session.Entry{Role: session.Agent}
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		m.InFlight.Thinking = false
		m.Progress.Mode = stateComplete
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
			return m, func() tea.Msg { return queuedTurnMsg{text: queued} }
		}
		return m, m.awaitSessionEvent()

	case session.ThinkingDelta:
		m.InFlight.ReasonBuf += msg.Delta
		return m, m.awaitSessionEvent()

	case session.AgentDelta:
		m.Progress.Mode = stateStreaming
		if m.InFlight.Pending == nil {
			m.InFlight.Pending = &session.Entry{Role: session.Agent}
		}
		m.InFlight.Pending.Content += msg.Delta
		m.InFlight.StreamBuf = m.InFlight.Pending.Content
		return m, m.awaitSessionEvent()

	case session.AgentMessage:
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Agent {
			if msg.Message != "" {
				m.InFlight.Pending.Content = msg.Message
			}
			m.InFlight.Pending.Reasoning = m.InFlight.ReasonBuf
			entry := *m.InFlight.Pending
			m.InFlight.Pending = nil
			m.InFlight.StreamBuf = ""
			m.InFlight.ReasonBuf = ""
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
		m.Progress.Mode = stateWorking
		m.Progress.LastToolUseID = session.ShortID()
		m.InFlight.Pending = &session.Entry{
			Role:  session.Tool,
			Title: FormatToolTitle(msg.ToolName, msg.Args),
		}
		if err := m.persistEntry("persist tool use", storage.ToolUse{
			Type: "tool_use",
			ID:   m.Progress.LastToolUseID,
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
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Tool {
			m.InFlight.Pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ToolResult:
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Tool {
			m.InFlight.Pending.Content = msg.Result
			m.InFlight.Pending.IsError = msg.Error != nil
			entry := *m.InFlight.Pending
			m.InFlight.Pending = nil

			if err := m.persistEntry("persist tool result", storage.ToolResult{
				Type:      "tool_result",
				ToolUseID: m.Progress.LastToolUseID,
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
			ToolUseID: m.Progress.LastToolUseID,
			Content:   content,
			IsError:   !msg.Passed,
			TS:        now(),
		}); err != nil {
			return m, tea.Sequence(m.printEntries(entry), persistErrorCmd("persist verification result", err))
		}
		return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())

	case session.ApprovalRequest:
		m.Approval.Pending = &msg
		m.Progress.Mode = stateApproval
		m.InFlight.Thinking = false
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		m.InFlight.Pending = &session.Entry{
			Role:    session.Subagent,
			Title:   msg.AgentName,
			Content: msg.Query,
		}
		m.Progress.Mode = stateWorking
		return m, m.awaitSessionEvent()

	case session.ChildStarted:
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Subagent {
			m.InFlight.Pending.Title = msg.AgentName
			m.Progress.Mode = stateWorking
		}
		return m, m.awaitSessionEvent()

	case session.ChildDelta:
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Subagent {
			m.InFlight.Pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ChildCompleted:
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Subagent {
			m.InFlight.Pending.Content = msg.Result
			entry := *m.InFlight.Pending
			m.InFlight.Pending = nil
			m.Progress.Mode = stateComplete
			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.ChildBlocked:
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Subagent {
			m.InFlight.Pending.Content = "BLOCKED: " + msg.Reason
			entry := *m.InFlight.Pending
			entry.IsError = false
			m.Progress.Mode = stateBlocked
			m.InFlight.Thinking = false
			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.ChildFailed:
		if m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Subagent {
			m.InFlight.Pending.Content = "ERROR: " + msg.Error
			m.InFlight.Pending.IsError = true
			entry := *m.InFlight.Pending
			m.InFlight.Pending = nil
			m.Progress.Mode = stateError
			m.Progress.LastError = "Subagent failed: " + msg.Error
			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.Error:
		m.InFlight.Pending = nil
		m.Approval.Pending = nil
		m.InFlight.QueuedTurns = nil
		m.InFlight.StreamBuf = ""
		m.InFlight.ReasonBuf = ""
		m.InFlight.Thinking = false
		m.Progress.Mode = stateError
		m.Progress.LastError = msg.Err.Error()
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
		entry := session.Entry{Role: session.System, Content: "Error: " + msg.Err.Error()}
		if err := m.persistEntry("persist session error", storage.System{
			Type:    "system",
			Content: entry.Content,
			TS:      now(),
		}); err != nil {
			return m, persistErrorCmd("persist session error", err)
		}
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

	m.Input.History = append(m.Input.History, text)
	m.Input.HistoryIdx = -1
	m.Input.HistoryDraft = ""

	userEntry := session.Entry{Role: session.User, Content: text}
	m.Input.Composer.Reset()
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

	m.Progress.Mode = stateIonizing
	m.InFlight.Thinking = true
	if err := m.Model.Session.SubmitTurn(context.Background(), text); err != nil {
		m, errCmd := m.handleSessionEvent(session.Error{Err: err})
		return m, tea.Batch(m.printEntries(userEntry), errCmd)
	}
	return m, m.printEntries(userEntry)
}
