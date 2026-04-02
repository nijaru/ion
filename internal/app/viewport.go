package app

import (
	"fmt"
	"strings"
	"unicode"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/session"
)

// Viewport handles the rendering of Plane A (committed scrollback) and Plane B (ephemeral in-flight content).
// Currently, it acts as an extension of the Model receiver, but is planned for componentization.
type Viewport struct{}

// renderPlaneB renders all ephemeral in-flight content.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	if m.InFlight.Pending == nil && m.Approval.Pending == nil && m.InFlight.ReasonBuf == "" {
		return ""
	}

	var b strings.Builder

	// Thinking/reasoning (dimmed, shown while generating)
	if m.InFlight.ReasonBuf != "" {
		b.WriteString(m.st.dim.Render("• Thinking..."))
		b.WriteString("\n")
		for _, line := range strings.Split(m.InFlight.ReasonBuf, "\n") {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(line))
			b.WriteString("\n")
		}
	}

	// Active in-flight entry (streaming agent, tool, or agent)
	if m.InFlight.Pending != nil {
		b.WriteString(m.renderPendingEntry(*m.InFlight.Pending))
		b.WriteString("\n")
	}

	// Approval prompt
	if m.Approval.Pending != nil {
		b.WriteString("\n")
		desc := m.Approval.Pending.Description
		if m.Approval.Pending.ToolName != "" {
			desc = fmt.Sprintf("%s: %s",
				FormatToolTitle(m.Approval.Pending.ToolName, m.Approval.Pending.Args),
				m.Approval.Pending.Description)
		}
		b.WriteString(m.st.warn.PaddingLeft(2).Render("Approve " + desc + "? (y/n/a)"))
		b.WriteString("\n")
	}

	return b.String()
}

// renderPendingEntry renders an in-flight entry for Plane B.
func (m Model) renderPendingEntry(e session.Entry) string {
	switch e.Role {
	case session.Agent:
		if e.Content == "" {
			return m.st.dim.PaddingLeft(2).Render("• ...")
		}
		return m.st.agent.Render("• " + e.Content)
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
			const maxLines = 10
			shown := lines
			if len(lines) > maxLines {
				shown = lines[len(lines)-maxLines:]
				b.WriteString(m.st.dim.PaddingLeft(4).Render(
					fmt.Sprintf("... (%d lines total)", len(lines))))
				b.WriteString("\n")
			}
			for _, l := range shown {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(l))
				b.WriteString("\n")
			}
		}
		return b.String()
	case session.Subagent:
		label := e.Title
		if label == "" {
			label = "subagent"
		}
		var b strings.Builder
		b.WriteString(m.st.subagent.Render("↳ " + label))
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

	case session.Agent:
		var b strings.Builder
		if e.Reasoning != "" {
			b.WriteString(m.st.system.Render("• Thinking"))
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Reasoning))
			b.WriteString("\n")
		}
		rendered := strings.TrimRightFunc(m.renderMarkdownContent(e.Content), unicode.IsSpace)
		if rendered == "" {
			b.WriteString(m.st.agent.Render("• "))
			return b.String()
		}
		lines := strings.Split(rendered, "\n")
		b.WriteString(m.st.agent.Render("• "))
		b.WriteString(lines[0])
		for _, line := range lines[1:] {
			b.WriteString("\n")
			b.WriteString(line)
		}
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

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
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

	case session.Subagent:
		label := e.Title
		if label == "" {
			label = "subagent"
		}
		var b strings.Builder
		b.WriteString(m.st.subagent.Render("↳ " + label))
		b.WriteString("\n")
		b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Content))
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

	case session.System:
		if strings.HasPrefix(e.Content, "Error: ") {
			return m.st.warn.Faint(true).Render("× " + e.Content)
		}
		return m.st.system.Render("• " + e.Content)

	default:
		return e.Content
	}
}

// progressLine renders the single-line progress indicator between Plane B and the composer.
func (m Model) progressLine() string {
	var line string
	switch m.Progress.Mode {
	case stateIonizing:
		line = m.Input.Spinner.View() + " Ionizing..."
		if stats := m.runningProgressParts(); len(stats) > 0 {
			line += " • " + strings.Join(stats, " • ")
		}
	case stateStreaming:
		line = m.Input.Spinner.View() + " Streaming..."
		if stats := m.runningProgressParts(); len(stats) > 0 {
			line += " • " + strings.Join(stats, " • ")
		}
	case stateWorking:
		line = m.Input.Spinner.View() + " Working..."
		if stats := m.runningProgressParts(); len(stats) > 0 {
			line += " • " + strings.Join(stats, " • ")
		}
	case stateComplete:
		line = m.st.success.Render("✓") + " Complete"
		if stats := m.completedProgressParts(); len(stats) > 0 {
			line += " • " + strings.Join(stats, " • ")
		}
	case stateApproval:
		line = m.st.warn.Render("⚠ Approval required")
	case stateCancelled:
		line = m.st.warn.Render("⚠ Canceled")
	case stateBlocked:
		line = m.st.warn.Render("⚠ Subagent blocked")
	case stateError:
		line = m.st.warn.Render(
			"× Error: " + strings.NewReplacer("\n", " ", "\r", " ").Replace(m.Progress.LastError),
		)
	default:
		if status := strings.TrimSpace(m.configurationStatus()); status != "" {
			line = m.st.warn.Render("• " + status)
		} else if status := strings.TrimSpace(m.Progress.Status); !isIdleStatus(status) && !isConfigurationStatus(status) {
			line = m.st.dim.Render("• " + status)
		} else {
			line = m.st.dim.Render("• Ready")
		}
	}
	if n := len(m.InFlight.QueuedTurns); n > 0 {
		line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
	}
	return fitLine(strings.TrimRight(line, " "), m.App.Width)
}

// renderDiff colorizes diff-format output.
// Uses plain output if the content doesn't look like a unified diff.
func (m Model) renderDiff(content string) string {
	lines := strings.Split(content, "\n")
	hasDiffMarkers := false
	for _, l := range lines {
		if strings.HasPrefix(l, "--- ") || strings.HasPrefix(l, "+++ ") ||
			strings.HasPrefix(l, "@@ ") {
			hasDiffMarkers = true
			break
		}
	}
	if !hasDiffMarkers {
		return content
	}

	var b strings.Builder
	for _, l := range lines {
		switch {
		case strings.HasPrefix(l, "+") && !strings.HasPrefix(l, "+++"):
			b.WriteString(m.st.added.Render(l))
		case strings.HasPrefix(l, "-") && !strings.HasPrefix(l, "---"):
			b.WriteString(m.st.removed.Render(l))
		case strings.HasPrefix(l, "@@ "):
			b.WriteString(m.st.cyan.Render(l))
		default:
			b.WriteString(m.st.dim.Render(l))
		}
		b.WriteString("\n")
	}
	return b.String()
}

// isWriteTool returns true if the tool title looks like a write/edit operation.
func isWriteTool(title string) bool {
	lower := strings.ToLower(title)
	for _, prefix := range []string{"write", "edit", "create"} {
		if strings.HasPrefix(lower, prefix+" ") || strings.HasPrefix(lower, prefix+"(") {
			return true
		}
	}
	return false
}

// FormatToolTitle attempts to extract the most important argument from a tool call's
// raw JSON string to create a more readable title.
func FormatToolTitle(name, args string) string {
	args = strings.TrimSpace(args)
	if args == "" || args == "{}" {
		return name
	}

	// Simple heuristic-based extraction to avoid full JSON overhead in the render loop.
	for _, key := range []string{"command", "file_path", "path", "pattern", "query"} {
		pattern := fmt.Sprintf("\"%s\":", key)
		if idx := strings.Index(args, pattern); idx != -1 {
			val := args[idx+len(pattern):]
			val = strings.TrimSpace(val)
			if strings.HasPrefix(val, "\"") {
				val = val[1:]
				if end := strings.Index(val, "\""); end != -1 {
					return fmt.Sprintf("%s %s", name, val[:end])
				}
			}
		}
	}

	return fmt.Sprintf("%s(%s)", name, args)
}

func fitLine(line string, width int) string {
	if width <= 0 {
		return ""
	}
	if ansi.StringWidth(line) <= width {
		return line
	}
	return ansi.Truncate(line, width, "…")
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
	if len(filtered) == 0 {
		return ""
	}
	return strings.Join(filtered, sep)
}
