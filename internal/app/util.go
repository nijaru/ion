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
		case "auto":
			return session.ModeYolo
		}
	}
	return session.ModeEdit
}

func configureModelSessionMode(agent session.AgentSession, mode session.Mode) {
	if agent == nil {
		return
	}
	agent.SetMode(mode)
	agent.SetAutoApprove(mode == session.ModeYolo)
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

func (m *Model) printHelp(content string) tea.Cmd {
	content = strings.TrimRight(content, "\n")
	if content == "" {
		return nil
	}
	lines := make([]string, 0, strings.Count(content, "\n")+2)
	if !m.App.PrintedTranscript {
		lines = append(lines, "")
		m.App.PrintedTranscript = true
	}
	for i, line := range strings.Split(content, "\n") {
		switch {
		case i == 0:
			lines = append(lines, m.st.cyan.Render(line))
		case isHelpSectionLine(line):
			lines = append(lines, m.st.cyan.Render(line))
		default:
			lines = append(lines, line)
		}
	}
	return printLinesCmd(lines...)
}

func isHelpSectionLine(line string) bool {
	switch strings.TrimSpace(line) {
	case "keys", "approval":
		return true
	default:
		return false
	}
}

func (m *Model) clearProgressError() {
	if m.Progress.Mode == stateError {
		m.Progress.Mode = stateReady
	}
	m.Progress.LastError = ""
}

func (m *Model) clearPendingAction() {
	m.Input.CtrlCPending = false
	m.Input.Pending = pendingActionNone
}

func (m *Model) armPendingAction(action pendingAction) tea.Cmd {
	m.Input.Pending = action
	switch action {
	case pendingActionQuitCtrlC, pendingActionQuitCtrlD:
		m.Input.CtrlCPending = true
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

func toolSurfaceSummary(surface backend.ToolSurface) string {
	if surface.Count == 0 {
		return "No tools registered"
	}
	mode := "eager"
	if surface.LazyEnabled {
		mode = fmt.Sprintf("lazy via search_tools above %d", surface.LazyThreshold)
	}
	names := strings.Join(surface.Names, ", ")
	sandbox := strings.TrimSpace(surface.Sandbox)
	if sandbox != "" {
		mode += "; sandbox " + sandbox
	}
	if names == "" {
		return fmt.Sprintf("Tools: %d (%s)", surface.Count, mode)
	}
	return fmt.Sprintf("Tools: %d (%s)\n%s", surface.Count, mode, names)
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

func isCompactingStatus(status string) bool {
	return strings.Contains(strings.ToLower(strings.TrimSpace(status)), "compacting")
}
