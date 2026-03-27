package app

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/session"
)

func (m Model) View() tea.View {
	if !m.ready {
		return tea.NewView("loading...")
	}

	var b strings.Builder

	startup := m.renderStartupBlock()
	if startup != "" {
		b.WriteString(startup)
		b.WriteString("\n")
	} else {
		// Blank line separates scrollback from dynamic area.
		b.WriteString("\n")
	}

	// Plane B — ephemeral in-flight content
	planeB := m.renderPlaneB()
	if planeB != "" {
		b.WriteString(planeB)
	}

	// Selection overlay
	if m.sessionPicker != nil {
		b.WriteString(m.renderSessionPicker())
		b.WriteString("\n")
	} else if m.picker != nil {
		b.WriteString(m.renderPicker())
		b.WriteString("\n")
	}

	// Blank line separates the startup/messages area from the progress line.
	b.WriteString("\n")

	// Progress line
	b.WriteString(m.progressLine())
	b.WriteString("\n")

	// Top separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.width))))
	b.WriteString("\n")

	// Composer
	b.WriteString(lipgloss.NewStyle().PaddingLeft(1).Render(m.composer.View()))
	b.WriteString("\n")

	// Bottom separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.width))))
	b.WriteString("\n")

	// Status line
	b.WriteString(m.statusLine())

	return tea.NewView(b.String())
}

// renderPlaneB renders all ephemeral in-flight content.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	if m.pending == nil && m.pendingApproval == nil && m.reasonBuf == "" {
		return ""
	}

	var b strings.Builder

	// Thinking/reasoning (dimmed, shown while generating)
	if m.reasonBuf != "" {
		b.WriteString(m.st.dim.Render("  • Thinking..."))
		b.WriteString("\n")
		for _, line := range strings.Split(m.reasonBuf, "\n") {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(line))
			b.WriteString("\n")
		}
	}

	// Active in-flight entry (streaming assistant, tool, or agent)
	if m.pending != nil {
		b.WriteString(m.renderPendingEntry(*m.pending))
		b.WriteString("\n")
	}

	// Approval prompt
	if m.pendingApproval != nil {
		b.WriteString("\n")
		desc := m.pendingApproval.Description
		if m.pendingApproval.ToolName != "" {
			desc = fmt.Sprintf("%s(%s): %s",
				m.pendingApproval.ToolName,
				m.pendingApproval.Args,
				m.pendingApproval.Description)
		}
		b.WriteString(m.st.warn.PaddingLeft(2).Render("Approve " + desc + "? (y/n)"))
		b.WriteString("\n")
	}

	return b.String()
}

func (m Model) renderPicker() string {
	items := pickerDisplayItems(m.picker)
	if m.picker == nil {
		return ""
	}

	const maxVisible = 8
	start := 0
	if len(items) > maxVisible {
		start = m.picker.index - maxVisible/2
		if start < 0 {
			start = 0
		}
		if end := start + maxVisible; end > len(items) {
			start = len(items) - maxVisible
		}
	}
	end := start + maxVisible
	if end > len(items) {
		end = len(items)
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.st.cyan.PaddingLeft(2).Render(m.picker.title))
	b.WriteString("\n")
	if m.picker.query != "" {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("filter: " + m.picker.query))
		b.WriteString("\n")
	}
	b.WriteString(m.st.dim.PaddingLeft(2).Render("type to filter • ↑/↓ navigate • enter select • esc cancel"))
	b.WriteString("\n")
	if len(items) == 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("No matching items"))
		b.WriteString("\n")
		return b.String()
	}
	if start > 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
		b.WriteString("\n")
	}
	for i := start; i < end; i++ {
		item := items[i]
		line := item.Label
		if item.Detail != "" {
			line += " • " + item.Detail
		}
		if i == m.picker.index {
			b.WriteString(m.st.cyan.PaddingLeft(2).Render("› " + line))
		} else {
			b.WriteString(m.st.dim.PaddingLeft(2).Render("  " + line))
		}
		b.WriteString("\n")
	}
	if end < len(items) {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
		b.WriteString("\n")
	}
	return b.String()
}

// renderPendingEntry renders an in-flight entry for Plane B.
func (m Model) renderPendingEntry(e session.Entry) string {
	switch e.Role {
	case session.Assistant:
		if e.Content == "" {
			return m.st.dim.PaddingLeft(2).Render("• ...")
		}
		return m.st.assistant.Render("• " + e.Content)
	case session.Tool:
		label := e.Title
		if label == "" {
			label = "tool"
		}
		var b strings.Builder
		b.WriteString(m.st.tool.Render("• " + label))
		if e.Content != "" {
			b.WriteString("\n")
			lines := strings.Split(strings.TrimRight(e.Content, "\n"), "\n")
			shown := lines
			if len(lines) > 10 {
				shown = lines[:10]
			}
			for _, l := range shown {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(l))
				b.WriteString("\n")
			}
			if len(lines) > 10 {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(
					fmt.Sprintf("... (%d more lines)", len(lines)-10)))
				b.WriteString("\n")
			}
		}
		return b.String()
	case session.Agent:
		label := e.Title
		if label == "" {
			label = "agent"
		}
		var b strings.Builder
		b.WriteString(m.st.agent.Render("↳ " + label))
		if e.Content != "" {
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Content))
		}
		return b.String()
	default:
		return e.Content
	}
}

// renderEntry formats a completed entry for tea.Printf scrollback commit.
func (m Model) renderEntry(e session.Entry) string {
	switch e.Role {
	case session.User:
		return m.st.user.Render("› " + e.Content)

	case session.Assistant:
		var b strings.Builder
		if e.Reasoning != "" {
			b.WriteString(m.st.system.Render("• Thinking"))
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Reasoning))
			b.WriteString("\n")
		}
		rendered := m.renderMarkdownContent(e.Content)
		if rendered == "" {
			b.WriteString(m.st.assistant.Render("• "))
			return b.String()
		}
		lines := strings.Split(rendered, "\n")
		b.WriteString(m.st.assistant.Render("• "))
		b.WriteString(lines[0])
		for _, line := range lines[1:] {
			b.WriteString("\n")
			b.WriteString("  ")
			b.WriteString(line)
		}
		return b.String()

	case session.Tool:
		label := e.Title
		if label == "" {
			label = "tool"
		}
		var labelStr string
		if e.IsError {
			labelStr = m.st.warn.Render("✗ " + label)
		} else {
			labelStr = m.st.tool.Render("• " + label)
		}
		if e.Content == "" {
			return labelStr
		}
		content := e.Content
		if isWriteTool(e.Title) {
			content = m.renderDiff(content)
		}
		var b strings.Builder
		b.WriteString(labelStr)
		b.WriteString("\n")
		lines := strings.Split(strings.TrimRight(content, "\n"), "\n")
		shown := lines
		if len(lines) > 10 {
			shown = lines[:10]
		}
		for _, l := range shown {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(l))
			b.WriteString("\n")
		}
		if len(lines) > 10 {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(
				fmt.Sprintf("... (%d more lines)", len(lines)-10)))
		}
		return b.String()

	case session.Agent:
		label := e.Title
		if label == "" {
			label = "agent"
		}
		var b strings.Builder
		b.WriteString(m.st.agent.Render("↳ " + label))
		b.WriteString("\n")
		b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Content))
		return b.String()

	case session.System:
		return m.st.system.Render("  " + e.Content)

	default:
		return e.Content
	}
}

// progressLine renders the single-line progress indicator between Plane B and the composer.
func (m Model) progressLine() string {
	var line string
	switch m.progress {
	case stateIonizing:
		line = m.st.cyan.Render("  " + m.spinner.View() + " Ionizing...")
	case stateStreaming:
		line = m.st.cyan.Render("  " + m.spinner.View() + " Streaming...")
	case stateWorking:
		line = m.st.cyan.Render("  " + m.spinner.View() + " Working...")
	case stateApproval:
		line = m.st.warn.Render("  ⚠ Approval required")
	case stateCancelled:
		line = m.st.dim.Render("  • Cancelled")
	case stateError:
		line = m.st.warn.Render("  ✗ Error: " + strings.NewReplacer("\n", " ", "\r", " ").Replace(m.lastError))
	default:
		line = m.st.dim.Render("  • Ready")
	}
	return fitLine(line, m.width)
}

// headerLine returns the workspace line shown below the startup banner.
func (m Model) headerLine() string {
	sep := m.st.dim.Render(" • ")

	home, _ := os.UserHomeDir()
	dir := m.workdir
	if home != "" && strings.HasPrefix(dir, home) {
		dir = "~" + dir[len(home):]
	}

	pathParts := []string{m.st.dim.Render(dir)}
	if m.branch != "" {
		pathParts = append(pathParts, m.st.dim.Render(m.branch))
	}
	return strings.Join(pathParts, sep)
}

// statusLine renders the bottom info bar.
func (m Model) statusLine() string {
	sep := m.st.cyan.Render(" • ")

	modeLabel := ifthen(m.mode == modeWrite,
		m.st.modeWrite.Render("[WRITE]"),
		m.st.modeRead.Render("[READ]"),
	)

	provider := ""
	if value := m.backend.Provider(); value != "" {
		provider = m.st.dim.Render(value)
	}
	model := ""
	if value := m.backend.Model(); value != "" {
		model = value
	}
	dir := m.st.dim.Render("./" + filepath.Base(m.workdir))
	branch := ""
	if m.branch != "" {
		branch = m.st.cyan.Render(m.branch)
	}

	total := m.tokensSent + m.tokensReceived
	limit := m.backend.ContextLimit()
	var usage string
	if total > 0 && limit > 0 {
		pct := (total * 100) / limit
		usage = m.st.cyan.Render(fmt.Sprintf("%dk/%dk (%d%%)", total/1000, limit/1000, pct))
	} else if total > 0 {
		usage = fmt.Sprintf("%dk tokens", total/1000)
	}

	cost := ""
	if m.totalCost > 0 {
		cost = fmt.Sprintf("$%.3f", m.totalCost)
	}

	candidates := [][]string{
		{modeLabel, provider, model, usage, cost, dir, branch},
		{modeLabel, provider, model, usage, cost, branch},
		{modeLabel, provider, model, usage, cost},
		{modeLabel, model, usage, cost},
	}
	for _, segments := range candidates {
		line := joinLineSegments(sep, segments...)
		if lipgloss.Width(line) <= m.width {
			return line
		}
	}

	return fitLine(joinLineSegments(sep, modeLabel, model, usage, cost), m.width)
}

// layout recomputes widget dimensions based on current terminal size.
func (m *Model) layout() {
	m.composer.SetWidth(max(20, m.width-4))
	m.composer.SetHeight(clamp(m.composer.LineCount(), minComposerHeight, maxComposerHeight))
}

func clamp(v, low, high int) int {
	if v < low {
		return low
	}
	if v > high {
		return high
	}
	return v
}

func joinLineSegments(sep string, segments ...string) string {
	filtered := make([]string, 0, len(segments))
	for _, segment := range segments {
		if segment != "" {
			filtered = append(filtered, segment)
		}
	}
	return "  " + strings.Join(filtered, sep)
}

func fitLine(line string, width int) string {
	if width <= 0 {
		return ""
	}
	if lipgloss.Width(line) <= width {
		return line
	}
	return ansi.Truncate(line, width, "…")
}
