package app

import (
	"context"
	"fmt"
	"strings"

	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// handleKey processes keyboard input.
func (m Model) handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
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
			return m, tea.Printf("%s\n", m.renderEntry(notice))
		}
	}

	switch msg.String() {
	case "ctrl+p":
		m.ctrlCPending = false
		m.escPending = false
		return m, m.openProviderPicker()

	case "ctrl+m":
		m.ctrlCPending = false
		m.escPending = false
		return m, m.openModelPicker()

	case "ctrl+c":
		if m.ctrlCPending || m.composer.Value() == "" {
			return m, tea.Quit
		}
		m.ctrlCPending = true
		m.composer.Reset()
		m.relayoutComposer()
		m.escPending = false
		return m, nil

	case "esc":
		m.ctrlCPending = false
		if m.thinking {
			m.session.CancelTurn(context.Background())
			m.thinking = false
			m.progress = stateCancelled
			m.pending = nil
			m.streamBuf = ""
			m.reasonBuf = ""
			return m, nil
		}
		if m.escPending {
			m.composer.Reset()
			m.relayoutComposer()
			m.escPending = false
			return m, nil
		}
		m.escPending = true
		return m, nil

	case "shift+tab":
		m.ctrlCPending = false
		m.escPending = false
		if m.mode == modeWrite {
			m.mode = modeRead
		} else {
			m.mode = modeWrite
		}
		return m, nil

	case "enter":
		m.ctrlCPending = false
		m.escPending = false
		text := strings.TrimSpace(m.composer.Value())
		if text == "" || m.thinking {
			return m, nil
		}

		m.history = append(m.history, text)
		m.historyIdx = -1
		m.historyDraft = ""

		userEntry := session.Entry{Role: session.User, Content: text}
		m.composer.Reset()
		m.relayoutComposer()

		if m.storage != nil {
			if err := m.storage.Append(context.Background(), storage.User{
				Type:    "user",
				Content: text,
				TS:      now(),
			}); err != nil {
				return m, tea.Batch(
					tea.Printf("%s\n", m.renderEntry(userEntry)),
					persistErrorCmd("persist user input", err),
				)
			}
		}

		if strings.HasPrefix(text, "/") {
			cmd := m.handleCommand(text)
			return m, tea.Batch(tea.Printf("%s\n", m.renderEntry(userEntry)), cmd)
		}

		m.progress = stateIonizing
		m.thinking = true
		m.session.SubmitTurn(context.Background(), text)
		return m, tea.Printf("%s\n", m.renderEntry(userEntry))

	case "shift+enter":
		m.ctrlCPending = false
		m.escPending = false
		var cmd tea.Cmd
		m.composer, cmd = m.composer.Update(msg)
		m.layout()
		return m, cmd

	case "up":
		m.ctrlCPending = false
		m.escPending = false
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
		m.ctrlCPending = false
		m.escPending = false
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
		m.ctrlCPending = false
		m.escPending = false
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
		if m.storage != nil {
			if err := m.storage.Append(context.Background(), storage.Status{
				Type:   "status",
				Status: msg.Status,
				TS:     now(),
			}); err != nil {
				return m, persistErrorCmd("persist status", err)
			}
		}
		return m, m.awaitSessionEvent()

	case session.TokenUsage:
		m.tokensSent += msg.Input
		m.tokensReceived += msg.Output
		m.totalCost += msg.Cost
		if m.storage != nil {
			if err := m.storage.Append(context.Background(), storage.TokenUsage{
				Type:   "token_usage",
				Input:  msg.Input,
				Output: msg.Output,
				Cost:   msg.Cost,
				TS:     now(),
			}); err != nil {
				return m, persistErrorCmd("persist token usage", err)
			}
		}
		return m, m.awaitSessionEvent()

	case session.TurnStarted:
		m.thinking = true
		m.progress = stateIonizing
		m.pending = &session.Entry{Role: session.Assistant}
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		m.thinking = false
		m.progress = stateReady
		return m, m.awaitSessionEvent()

	case session.ThinkingDelta:
		m.reasonBuf += msg.Delta
		return m, m.awaitSessionEvent()

	case session.AssistantDelta:
		m.progress = stateStreaming
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.Assistant}
		}
		m.pending.Content += msg.Delta
		m.streamBuf = m.pending.Content
		return m, m.awaitSessionEvent()

	case session.AssistantMessage:
		if m.pending != nil && m.pending.Role == session.Assistant {
			if msg.Message != "" {
				m.pending.Content = msg.Message
			}
			m.pending.Reasoning = m.reasonBuf
			entry := *m.pending
			m.pending = nil
			m.streamBuf = ""
			m.reasonBuf = ""

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
				if err := m.storage.Append(context.Background(), storage.Assistant{
					Type:    "assistant",
					Content: blocks,
					TS:      now(),
				}); err != nil {
					return m, tea.Batch(
						tea.Printf("%s\n", m.renderEntry(entry)),
						persistErrorCmd("persist assistant response", err),
					)
				}
			}
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
		}
		return m, m.awaitSessionEvent()

	case session.ToolCallStarted:
		m.progress = stateWorking
		m.lastToolUseID = session.ShortID()
		if m.storage != nil {
			if err := m.storage.Append(context.Background(), storage.ToolUse{
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
		}
		m.pending = &session.Entry{
			Role:  session.Tool,
			Title: fmt.Sprintf("%s(%s)", msg.ToolName, msg.Args),
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

			if m.storage != nil {
				if err := m.storage.Append(context.Background(), storage.ToolResult{
					Type:      "tool_result",
					ToolUseID: m.lastToolUseID,
					Content:   msg.Result,
					IsError:   msg.Error != nil,
					TS:        now(),
				}); err != nil {
					return m, tea.Batch(
						tea.Printf("%s\n", m.renderEntry(entry)),
						persistErrorCmd("persist tool result", err),
					)
				}
			}
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
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
		if m.storage != nil {
			if err := m.storage.Append(context.Background(), storage.ToolResult{
				Type:      "tool_result",
				ToolUseID: m.lastToolUseID,
				Content:   content,
				IsError:   !msg.Passed,
				TS:        now(),
			}); err != nil {
				return m, persistErrorCmd("persist verification result", err)
			}
		}
		return m, tea.Batch(
			tea.Printf("%s\n", m.renderEntry(entry)),
			m.awaitSessionEvent(),
		)

	case session.ApprovalRequest:
		m.pendingApproval = &msg
		m.progress = stateApproval
		m.thinking = false
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		m.pending = &session.Entry{
			Role:    session.Agent,
			Title:   msg.AgentName,
			Content: msg.Query,
		}
		return m, m.awaitSessionEvent()

	case session.ChildStarted:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Title = msg.AgentName
		}
		return m, m.awaitSessionEvent()

	case session.ChildDelta:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ChildCompleted:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content = msg.Result
			entry := *m.pending
			m.pending = nil
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
		}
		return m, m.awaitSessionEvent()

	case session.ChildFailed:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content = "ERROR: " + msg.Error
			m.pending.IsError = true
			entry := *m.pending
			m.pending = nil
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
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
		return m, m.awaitSessionEvent()
	}

	return m, m.awaitSessionEvent()
}

func persistErrorCmd(action string, err error) tea.Cmd {
	return func() tea.Msg {
		return session.Error{Err: fmt.Errorf("%s: %w", action, err)}
	}
}

// Ensure textarea.Blink is referenced (avoids unused import if Focus() is the only use).
var _ = textarea.Blink
