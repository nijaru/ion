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

// handleKey is the source of truth for core TUI hotkey semantics.
//
// Keep these aligned with the intended inline-agent UX:
//   - Enter sends; Shift+Enter and Alt+Enter insert a newline.
//   - Esc cancels an in-flight turn; when idle, double-tap Esc clears input.
//   - Ctrl+C clears non-empty input; when idle and empty, double-tap quits.
//   - Ctrl+D never clears input; when idle and empty, double-tap quits.
//   - Ctrl+C and Ctrl+D do not cancel running turns.
func (m Model) handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.sessionPicker != nil {
		return m.handleSessionPickerKey(msg)
	}
	if m.picker != nil {
		return m.handlePickerKey(msg)
	}

	// Approval gate: y/n consumed before any other handling
	if m.pendingApproval != nil {
		switch msg.String() {
		case "y", "n":
			approved := msg.String() == "y"
			reqID := m.pendingApproval.RequestID
			desc := m.pendingApproval.Description
			m.pendingApproval = nil
			m.progress = stateReady

			label := ifthen(approved, "Approved", "Denied")
			notice := session.Entry{Role: session.System, Content: label + ": " + desc}
			m.session.Approve(context.Background(), reqID, approved)
			return m, m.printEntries(notice)
		}
	}

	switch msg.String() {
	case "ctrl+p":
		m.clearPendingAction()
		return m.openProviderPicker()

	case "ctrl+m":
		m.clearPendingAction()
		return m.openModelPicker()

	case "ctrl+t":
		m.clearPendingAction()
		return m.openThinkingPicker()

	case "ctrl+c":
		m.escPending = false
		if m.composer.Value() != "" {
			m.clearPendingAction()
			m.composer.Reset()
			m.relayoutComposer()
			return m, nil
		}
		if m.thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.ctrlCPending {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlC)

	case "ctrl+d":
		m.escPending = false
		if m.composer.Value() != "" || m.thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.ctrlCPending {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlD)

	case "esc":
		m.ctrlCPending = false
		if m.thinking {
			m.session.CancelTurn(context.Background())
			m.thinking = false
			m.progress = stateCancelled
			m.pending = nil
			m.streamBuf = ""
			m.reasonBuf = ""
			m.clearPendingAction()
			return m, nil
		}
		if m.composer.Value() == "" {
			m.clearPendingAction()
			return m, nil
		}
		if m.escPending {
			m.composer.Reset()
			m.relayoutComposer()
			m.clearPendingAction()
			return m, nil
		}
		return m, m.armPendingAction(pendingActionClearEsc)

	case "shift+tab":
		m.clearPendingAction()
		if m.mode == session.ModeWrite {
			m.mode = session.ModeRead
		} else {
			m.mode = session.ModeWrite
		}
		m.session.SetMode(m.mode)
		return m, nil

	case "enter":
		m.clearPendingAction()
		text := strings.TrimSpace(m.composer.Value())
		if text == "" {
			return m, nil
		}
		if m.thinking {
			m.queuedTurn = text
			m.composer.Reset()
			m.relayoutComposer()
			return m, m.printEntries(session.Entry{Role: session.System, Content: "Queued follow-up"})
		}

		return m.submitText(text)

	case "shift+enter", "alt+enter":
		m.clearPendingAction()
		var cmd tea.Cmd
		m.composer, cmd = m.composer.Update(msg)
		m.layout()
		return m, cmd

	case "up":
		m.clearPendingAction()
		if m.composer.Line() == 0 && len(m.history) > 0 {
			if m.historyIdx == -1 {
				m.historyDraft = m.composer.Value()
				m.historyIdx = len(m.history) - 1
			} else if m.historyIdx > 0 {
				m.historyIdx--
			}
			m.composer.SetValue(m.history[m.historyIdx])
			m.relayoutComposer()
			return m, nil
		}
		var cmd tea.Cmd
		m.composer, cmd = m.composer.Update(msg)
		return m, cmd

	case "down":
		m.clearPendingAction()
		if m.historyIdx != -1 {
			if m.historyIdx < len(m.history)-1 {
				m.historyIdx++
				m.composer.SetValue(m.history[m.historyIdx])
				m.relayoutComposer()
			} else {
				m.historyIdx = -1
				m.composer.SetValue(m.historyDraft)
				m.historyDraft = ""
				m.relayoutComposer()
			}
			return m, nil
		}
		var cmd tea.Cmd
		m.composer, cmd = m.composer.Update(msg)
		return m, cmd

	default:
		m.clearPendingAction()
	}

	// Pass all other keys to textarea (Ctrl+A/E/W/U/K, Alt+B/F, etc.)
	var cmd tea.Cmd
	m.composer, cmd = m.composer.Update(msg)
	if m.ready {
		m.layout()
	}
	return m, cmd
}

func (m *Model) relayoutComposer() {
	if m.ready {
		m.layout()
	}
}

// handleSessionEvent processes events from the agent session channel.
func (m Model) handleSessionEvent(ev session.Event) (Model, tea.Cmd) {
	switch msg := ev.(type) {
	case session.StatusChanged:
		m.status = msg.Status
		var persistCmd tea.Cmd
		if m.storage != nil {
			persistCmd = m.persistCmd("persist status", storage.Status{
				Type:   "status",
				Status: msg.Status,
				TS:     now(),
			})
		}
		return m, tea.Sequence(persistCmd, m.awaitSessionEvent())

	case session.TokenUsage:
		m.tokensSent += msg.Input
		m.tokensReceived += msg.Output
		m.totalCost += msg.Cost
		m.currentTurnInput += msg.Input
		m.currentTurnOutput += msg.Output
		m.currentTurnCost += msg.Cost
		var persistCmd tea.Cmd
		if m.storage != nil {
			persistCmd = m.persistCmd("persist token usage", storage.TokenUsage{
				Type:   "token_usage",
				Input:  msg.Input,
				Output: msg.Output,
				Cost:   msg.Cost,
				TS:     now(),
			})
		}
		return m, tea.Sequence(persistCmd, m.awaitSessionEvent())

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

			if m.storage != nil {
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
				persistCmd := m.persistCmd("persist agent response", storage.Agent{
					Type:    "agent",
					Content: blocks,
					TS:      now(),
				})
				return m, tea.Sequence(m.printEntries(entry), persistCmd, m.awaitSessionEvent())
			}
			return m, tea.Sequence(m.printEntries(entry), m.awaitSessionEvent())
		}
		return m, m.awaitSessionEvent()

	case session.ToolCallStarted:
		m.progress = stateWorking
		m.lastToolUseID = session.ShortID()
		var persistCmd tea.Cmd
		if m.storage != nil {
			persistCmd = m.persistCmd("persist tool use", storage.ToolUse{
				Type: "tool_use",
				ID:   m.lastToolUseID,
				Name: msg.ToolName,
				Input: map[string]string{
					"args": msg.Args,
				},
				TS: now(),
			})
		}
		m.pending = &session.Entry{
			Role:  session.Tool,
			Title: formatToolTitle(msg.ToolName, msg.Args),
		}
		return m, tea.Sequence(persistCmd, m.awaitSessionEvent())

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

			var persistCmd tea.Cmd
			if m.storage != nil {
				persistCmd = m.persistCmd("persist tool result", storage.ToolResult{
					Type:      "tool_result",
					ToolUseID: m.lastToolUseID,
					Content:   msg.Result,
					IsError:   msg.Error != nil,
					TS:        now(),
				})
			}
			return m, tea.Sequence(m.printEntries(entry), persistCmd, m.awaitSessionEvent())
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
		var persistCmd tea.Cmd
		if m.storage != nil {
			persistCmd = m.persistCmd("persist verification result", storage.ToolResult{
				Type:      "tool_result",
				ToolUseID: m.lastToolUseID,
				Content:   content,
				IsError:   !msg.Passed,
				TS:        now(),
			})
		}
		return m, tea.Sequence(m.printEntries(entry), persistCmd, m.awaitSessionEvent())

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

func (m Model) persistCmd(action string, entry any) tea.Cmd {
	if m.storage == nil {
		return nil
	}
	return func() tea.Msg {
		if err := m.storage.Append(context.Background(), entry); err != nil {
			return session.Error{Err: fmt.Errorf("%s: %w", action, err)}
		}
		return nil
	}
}

func (m Model) submitText(text string) (Model, tea.Cmd) {
	m.history = append(m.history, text)
	m.historyIdx = -1
	m.historyDraft = ""

	userEntry := session.Entry{Role: session.User, Content: text}
	m.composer.Reset()
	m.relayoutComposer()

	var persistCmd tea.Cmd
	if m.storage != nil {
		persistCmd = m.persistCmd("persist user input", storage.User{
			Type:    "user",
			Content: text,
			TS:      now(),
		})
	}

	if strings.HasPrefix(text, "/") {
		m, cmd := m.handleCommand(text)
		return m, tea.Sequence(m.printEntries(userEntry), persistCmd, cmd)
	}

	m.progress = stateIonizing
	m.thinking = true
	m.session.SubmitTurn(context.Background(), text)
	return m, tea.Sequence(m.printEntries(userEntry), persistCmd)
}
