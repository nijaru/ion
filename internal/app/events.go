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
			if err := m.Model.Session.Approve(context.Background(), reqID, approved); err != nil {
				return m, persistErrorCmd("send approval", err)
			}
			return m, m.printEntries(notice)
		case "a":
			reqID := m.Approval.Pending.RequestID
			toolName := m.Approval.Pending.ToolName
			desc := m.Approval.Pending.Description
			m.Approval.Pending = nil
			m.Progress.Mode = stateReady

			m.Model.Session.AllowCategory(toolName)
			notice := session.Entry{Role: session.System, Content: "Always: " + desc}
			if err := m.Model.Session.Approve(context.Background(), reqID, true); err != nil {
				return m, persistErrorCmd("send approval", err)
			}
			return m, m.printEntries(notice)
		}
	}

	switch msg.String() {
	case "ctrl+m":
		m.clearPendingAction()
		if m.activePreset() == presetFast {
			return m.switchPresetCommand(presetPrimary)
		}
		return m.switchPresetCommand(presetFast)

	case "ctrl+t":
		m.clearPendingAction()
		return m.openThinkingPicker()

	case "ctrl+c":
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
		if m.Input.Composer.Value() != "" || m.InFlight.Thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.CtrlCPending {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlD)

	case "?":
		if strings.TrimSpace(m.Input.Composer.Value()) == "" {
			m.clearPendingAction()
			return m, m.printHelp(helpText())
		}

	case "esc":
		if m.InFlight.Thinking {
			m.Model.Session.CancelTurn(context.Background())
			m.InFlight.Thinking = false
			m.Progress.Mode = stateCancelled
			m.InFlight.Pending = nil
			m.InFlight.PendingTools = nil
			m.InFlight.QueuedTurns = nil
			m.InFlight.StreamBuf = ""
			m.InFlight.ReasonBuf = ""
			m.clearPendingAction()
			return m, nil
		}
		m.clearPendingAction()
		return m, nil

	case "shift+tab":
		m.clearPendingAction()
		switch m.Mode {
		case session.ModeRead:
			next, cmd := m.setModeCommand(session.ModeEdit)
			return next, cmd
		case session.ModeEdit:
			next, cmd := m.setModeCommand(session.ModeRead)
			return next, cmd
		default:
			next, cmd := m.setModeCommand(session.ModeEdit)
			return next, cmd
		}

	case "tab":
		if next, cmd, ok := m.completeSlashCommand(); ok {
			return next, cmd
		}

	case "enter":
		m.clearPendingAction()
		text := strings.TrimSpace(m.Input.Composer.Value())
		if text == "" {
			return m, nil
		}
		if strings.HasPrefix(text, "/") && commandAllowedDuringTurn(text) {
			return m.submitText(text)
		}
		if m.InFlight.Thinking || m.Progress.Compacting {
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

	case "up", "ctrl+p":
		m.clearPendingAction()
		if m.Input.Composer.Line() == 0 && len(m.Input.History) > 0 {
			if m.Input.HistoryIdx == -1 {
				m.Input.HistoryDraft = m.Input.Composer.Value()
				m.Input.HistoryIdx = len(m.Input.History) - 1
				m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
				m.relayoutComposer()
				return m, nil
			} else if m.Input.HistoryIdx > 0 {
				m.Input.HistoryIdx--
				m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
				m.relayoutComposer()
				return m, nil
			}
		}
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		return m, cmd

	case "down", "ctrl+n":
		m.clearPendingAction()
		if m.Input.Composer.Line() == m.Input.Composer.LineCount()-1 && m.Input.HistoryIdx != -1 {
			if m.Input.HistoryIdx < len(m.Input.History)-1 {
				m.Input.HistoryIdx++
				m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
				m.relayoutComposer()
				return m, nil
			} else {
				m.Input.HistoryIdx = -1
				m.Input.Composer.SetValue(m.Input.HistoryDraft)
				m.Input.HistoryDraft = ""
				m.relayoutComposer()
				return m, nil
			}
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

func (m Model) completeSlashCommand() (Model, tea.Cmd, bool) {
	text := m.Input.Composer.Value()
	if !strings.HasPrefix(text, "/") || strings.ContainsAny(text, " \t\r\n") {
		return m, nil, false
	}

	matches := matchingSlashCommands(text)
	switch len(matches) {
	case 0:
		return m, nil, true
	case 1:
		m.Input.Composer.SetValue(matches[0] + " ")
		m.relayoutComposer()
		return m, nil, true
	}

	prefix := commonPrefix(matches)
	if prefix != "" && prefix != text {
		m.Input.Composer.SetValue(prefix)
		m.relayoutComposer()
		return m, nil, true
	}

	return m, m.printEntries(session.Entry{
		Role:    session.System,
		Content: "Commands: " + strings.Join(matches, " "),
	}), true
}

func matchingSlashCommands(prefix string) []string {
	var matches []string
	for _, command := range slashCommands() {
		if strings.HasPrefix(command, prefix) {
			matches = append(matches, command)
		}
	}
	return matches
}

func commonPrefix(values []string) string {
	if len(values) == 0 {
		return ""
	}
	prefix := values[0]
	for _, value := range values[1:] {
		for !strings.HasPrefix(value, prefix) {
			prefix = prefix[:len(prefix)-1]
			if prefix == "" {
				return ""
			}
		}
	}
	return prefix
}

func (m *Model) relayoutComposer() {
	if m.App.Ready {
		m.layout()
	}
}
