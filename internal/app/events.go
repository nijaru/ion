package app

import (
	"context"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

// handleKey is the source of truth for core TUI hotkey semantics.
func (m Model) handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.sessionPicker != nil {
		return m.handleSessionPickerKey(msg)
	}
	if m.picker != nil {
		return m.handlePickerKey(msg)
	}

	// Approval gate: y/n/a consumed before any other handling
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
		case "a":
			reqID := m.pendingApproval.RequestID
			desc := m.pendingApproval.Description
			m.pendingApproval = nil
			m.progress = stateReady

			m.session.SetAutoApprove(true)
			notice := session.Entry{Role: session.System, Content: "Always: " + desc}
			m.session.Approve(context.Background(), reqID, true)
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
		switch m.mode {
		case session.ModeRead:
			m.mode = session.ModeEdit
		case session.ModeEdit:
			m.mode = session.ModeYolo
		default:
			m.mode = session.ModeRead
		}
		m.session.SetMode(m.mode)
		m.session.SetAutoApprove(m.mode == session.ModeYolo)
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
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Queued follow-up"},
			)
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
