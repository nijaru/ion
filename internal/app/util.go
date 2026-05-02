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

const (
	printSubmitHoldThreshold = 12
	printSubmitHoldBase      = 150 * time.Millisecond
	printSubmitHoldPerLine   = 15 * time.Millisecond
	printSubmitHoldMax       = 1 * time.Second
)

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

func backendSandboxSummary(b backend.Backend) string {
	summarizer, ok := b.(backend.ToolSummarizer)
	if !ok {
		return ""
	}
	return strings.TrimSpace(summarizer.ToolSurface().Sandbox)
}

func printLinesCmd(lines ...string) tea.Cmd {
	filtered := make([]string, 0, physicalLineCount(lines))
	for _, line := range lines {
		filtered = append(filtered, strings.Split(line, "\n")...)
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
	return printLinesCmd(m.RenderEntries(entries...)...)
}

func (m Model) RenderEntries(entries ...session.Entry) []string {
	lines := make([]string, 0, len(entries)*2)
	for _, entry := range entries {
		if len(lines) > 0 {
			lines = append(lines, "")
		}
		lines = append(lines, m.renderEntry(entry))
	}
	return lines
}

func (m *Model) printEntries(entries ...session.Entry) tea.Cmd {
	if len(entries) == 0 {
		return nil
	}
	lines := make([]string, 0, len(entries))
	m.App.PrintedTranscript = true
	lines = append(lines, m.RenderEntries(entries...)...)
	physicalLines := physicalLineCount(lines)
	m.holdEnterForLargePrint(physicalLines)
	return printLinesCmd(lines...)
}

func (m *Model) printHelp(content string) tea.Cmd {
	content = strings.TrimRight(content, "\n")
	if content == "" {
		return nil
	}
	lines := make([]string, 0, strings.Count(content, "\n")+1)
	m.App.PrintedTranscript = true
	for i, line := range strings.Split(content, "\n") {
		lines = append(lines, m.renderHelpLine(i, line))
	}
	physicalLines := physicalLineCount(lines)
	m.holdEnterForLargePrint(physicalLines)
	return printLinesCmd(lines...)
}

func physicalLineCount(lines []string) int {
	count := 0
	for _, line := range lines {
		count += strings.Count(line, "\n") + 1
	}
	return count
}

func (m *Model) holdEnterForLargePrint(lines int) {
	if lines < printSubmitHoldThreshold {
		return
	}
	delay := printSubmitHoldBase + time.Duration(lines)*printSubmitHoldPerLine
	if delay > printSubmitHoldMax {
		delay = printSubmitHoldMax
	}
	if delay > m.Input.PrintHoldDelay {
		m.Input.PrintHoldDelay = delay
	}
	m.Input.DelayNextEnter = true
}

func (m Model) printHoldActive() bool {
	return time.Now().Before(m.Input.PrintHoldUntil)
}

func (m Model) scheduleDeferredEnter() tea.Cmd {
	delay := time.Until(m.Input.PrintHoldUntil)
	if delay < 10*time.Millisecond {
		delay = 10 * time.Millisecond
	}
	return tea.Tick(delay, func(time.Time) tea.Msg {
		return deferredEnterMsg{}
	})
}

func (m Model) renderHelpLine(index int, line string) string {
	if index == 0 || isHelpSectionLine(line) {
		return m.st.cyan.Bold(true).Render(line)
	}
	if key, sep, detail, ok := splitHelpDetail(line); ok {
		return "  " + m.st.cyan.Render(key) + sep + detail
	}
	return line
}

func splitHelpDetail(line string) (string, string, string, bool) {
	if !strings.HasPrefix(line, "  ") {
		return "", "", "", false
	}
	trimmed := strings.TrimSpace(line)
	if trimmed == "" {
		return "", "", "", false
	}
	rest := strings.TrimLeft(line, " ")
	for i := 0; i < len(rest)-1; i++ {
		if rest[i] == ' ' && rest[i+1] == ' ' {
			key := strings.TrimSpace(rest[:i])
			j := i
			for j < len(rest) && rest[j] == ' ' {
				j++
			}
			sep := rest[i:j]
			detail := strings.TrimSpace(rest[j:])
			if key == "" || detail == "" {
				return "", "", "", false
			}
			return key, sep, detail, true
		}
	}
	return "", "", "", false
}

func isHelpSectionLine(line string) bool {
	switch strings.TrimSpace(line) {
	case "commands", "keys", "approval":
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
