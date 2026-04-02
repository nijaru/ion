package app

import (
	"context"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

// handleKey is the source of truth for core TUI hotkey semantics.
func (m Model) handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Session != nil {
		return m.handleSessionPickerKey(msg)
	}
	if m.Picker.Overlay != nil {
		return m.handlePickerKey(msg)
	}

	// Approval gate: y/n/a consumed before any other handling
	if m.Approval.Pending != nil {
		switch msg.String() {
		case "y", "n":
			approved := msg.String() == "y"
			reqID := m.Approval.Pending.RequestID
			desc := m.Approval.Pending.Description
			m.Approval.Pending = nil
			m.Progress.Mode = stateReady

			label := ifthen(approved, "Approved", "Denied")
			notice := session.Entry{Role: session.System, Content: label + ": " + desc}
			m.Model.Session.Approve(context.Background(), reqID, approved)
			return m, m.printEntries(notice)
		case "a":
			reqID := m.Approval.Pending.RequestID
			toolName := m.Approval.Pending.ToolName
			desc := m.Approval.Pending.Description
			m.Approval.Pending = nil
			m.Progress.Mode = stateReady

			m.Model.Session.AllowCategory(toolName)
			notice := session.Entry{Role: session.System, Content: "Always: " + desc}
			m.Model.Session.Approve(context.Background(), reqID, true)
			return m, m.printEntries(notice)
		}
	}

	switch msg.String() {
	case "ctrl+p":
		m.clearPendingAction()
		if m.activePreset() == presetFast {
			return m.switchPresetCommand(presetPrimary)
		}
		return m.switchPresetCommand(presetFast)

	case "ctrl+t":
		m.clearPendingAction()
		return m.openThinkingPicker()

	case "ctrl+c":
		m.Input.EscPending = false
		if m.Input.Composer.Value() != "" {
			m.clearPendingAction()
			m.Input.Composer.Reset()
			m.PasteMarkers = make(map[string]pasteMarker)
			m.relayoutComposer()
			return m, nil
		}
		if m.InFlight.Thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.CtrlCPending {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlC)

	case "ctrl+d":
		m.Input.EscPending = false
		if m.Input.Composer.Value() != "" || m.InFlight.Thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.CtrlCPending {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlD)

	case "esc":
		m.Input.CtrlCPending = false
		if m.InFlight.Thinking {
			if len(m.InFlight.QueuedTurns) > 0 {
				last := m.InFlight.QueuedTurns[len(m.InFlight.QueuedTurns)-1]
				m.InFlight.QueuedTurns = m.InFlight.QueuedTurns[:len(m.InFlight.QueuedTurns)-1]
				m.Input.Composer.SetValue(last)
				m.relayoutComposer()
				return m, nil
			}
			m.Model.Session.CancelTurn(context.Background())
			m.InFlight.Thinking = false
			m.Progress.Mode = stateCancelled
			m.InFlight.Pending = nil
			m.InFlight.StreamBuf = ""
			m.InFlight.ReasonBuf = ""
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.Composer.Value() == "" {
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.EscPending {
			m.Input.Composer.Reset()
			m.PasteMarkers = make(map[string]pasteMarker)
			m.relayoutComposer()
			m.clearPendingAction()
			return m, nil
		}
		return m, m.armPendingAction(pendingActionClearEsc)

	case "shift+tab":
		m.clearPendingAction()
		switch m.Mode {
		case session.ModeRead:
			m.Mode = session.ModeEdit
		case session.ModeEdit:
			m.Mode = session.ModeYolo
		default:
			m.Mode = session.ModeRead
		}
		m.Model.Session.SetMode(m.Mode)
		m.Model.Session.SetAutoApprove(m.Mode == session.ModeYolo)
		return m, nil

	case "enter":
		m.clearPendingAction()
		text := strings.TrimSpace(m.Input.Composer.Value())
		if text == "" {
			return m, nil
		}
		if m.InFlight.Thinking {
			m.InFlight.QueuedTurns = append(m.InFlight.QueuedTurns, text)
			m.Input.Composer.Reset()
			m.PasteMarkers = make(map[string]pasteMarker)
			m.relayoutComposer()
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Queued follow-up"},
			)
		}

		return m.submitText(text)

	case "shift+enter", "alt+enter":
		m.clearPendingAction()
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		m.layout()
		return m, cmd

	case "up":
		m.clearPendingAction()
		if m.Input.Composer.Line() == 0 && len(m.Input.History) > 0 {
			if m.Input.HistoryIdx == -1 {
				m.Input.HistoryDraft = m.Input.Composer.Value()
				m.Input.HistoryIdx = len(m.Input.History) - 1
			} else if m.Input.HistoryIdx > 0 {
				m.Input.HistoryIdx--
			}
			m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
			m.relayoutComposer()
			return m, nil
		}
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		return m, cmd

	case "down":
		m.clearPendingAction()
		if m.Input.HistoryIdx != -1 {
			if m.Input.HistoryIdx < len(m.Input.History)-1 {
				m.Input.HistoryIdx++
				m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
				m.relayoutComposer()
			} else {
				m.Input.HistoryIdx = -1
				m.Input.Composer.SetValue(m.Input.HistoryDraft)
				m.Input.HistoryDraft = ""
				m.relayoutComposer()
			}
			return m, nil
		}
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		return m, cmd

	default:
		m.clearPendingAction()
	}

	// Pass all other keys to textarea (Ctrl+A/E/W/U/K, Alt+B/F, etc.)
	var cmd tea.Cmd
	m.Input.Composer, cmd = m.Input.Composer.Update(msg)
	if m.App.Ready {
		m.layout()
	}
	return m, cmd
}

func (m *Model) relayoutComposer() {
	if m.App.Ready {
		m.layout()
	}
}
