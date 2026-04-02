package app

import (
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

func ifthen[T any](cond bool, a, b T) T {
	if cond {
		return a
	}
	return b
}

func now() int64 { return time.Now().Unix() }

func initialMode(_ backend.Bootstrap) session.Mode {
	if cfg, err := config.Load(); err == nil {
		switch config.ResolveDefaultMode(cfg.DefaultMode) {
		case "read":
			return session.ModeRead
		case "yolo":
			return session.ModeYolo
		}
	}
	return session.ModeEdit
}

func printLinesCmd(lines ...string) tea.Cmd {
	filtered := make([]string, 0, len(lines))
	for _, line := range lines {
		filtered = append(filtered, line)
	}
	if len(filtered) == 0 {
		return nil
	}
	return tea.Printf("%s\n", strings.Join(filtered, "\n"))
}

func printEntriesCmd(m Model, entries ...session.Entry) tea.Cmd {
	if len(entries) == 0 {
		return nil
	}
	lines := make([]string, 0, len(entries))
	for _, entry := range entries {
		lines = append(lines, m.renderEntry(entry))
	}
	return printLinesCmd(lines...)
}

func (m *Model) printEntries(entries ...session.Entry) tea.Cmd {
	if len(entries) == 0 {
		return nil
	}
	lines := make([]string, 0, len(entries)+1)
	if !m.App.PrintedTranscript {
		lines = append(lines, "")
		m.App.PrintedTranscript = true
	}
	for _, entry := range entries {
		lines = append(lines, m.renderEntry(entry))
	}
	return printLinesCmd(lines...)
}

func (m *Model) clearPendingAction() {
	m.Input.EscPending = false
	m.Input.CtrlCPending = false
	m.Input.Pending = pendingActionNone
}

func (m *Model) armPendingAction(action pendingAction) tea.Cmd {
	m.Input.Pending = action
	switch action {
	case pendingActionClearEsc:
		m.Input.EscPending = true
		m.Input.CtrlCPending = false
	case pendingActionQuitCtrlC, pendingActionQuitCtrlD:
		m.Input.CtrlCPending = true
		m.Input.EscPending = false
	default:
		m.clearPendingAction()
		return nil
	}
	return tea.Tick(pendingActionTimeout, func(time.Time) tea.Msg {
		return clearPendingMsg{action: action}
	})
}

func (m Model) pendingActionStatus() string {
	switch m.Input.Pending {
	case pendingActionQuitCtrlC:
		return "Press Ctrl+C again to quit"
	case pendingActionQuitCtrlD:
		return "Press Ctrl+D again to quit"
	case pendingActionClearEsc:
		return "Press Esc again to clear input"
	default:
		return ""
	}
}

func isConfigurationStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return false
	}
	lower := strings.ToLower(trimmed)
	return trimmed == noProviderConfiguredStatus() ||
		trimmed == noModelConfiguredStatus() ||
		strings.HasPrefix(lower, "provider and model are required")
}

func noProviderConfiguredStatus() string {
	return "No provider configured. Use /provider. Set ION_PROVIDER for scripts."
}

func noModelConfiguredStatus() string {
	return "No model configured. Use /model. Set ION_MODEL for scripts."
}

func compactCount(n int) string {
	if n >= 1000 {
		return fmt.Sprintf("%.1fk", float64(n)/1000.0)
	}
	return fmt.Sprintf("%d", n)
}

func isIdleStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return true
	}
	switch strings.ToLower(trimmed) {
	case "ready", "connected via canto", "connected via acp":
		return true
	default:
		return false
	}
}
