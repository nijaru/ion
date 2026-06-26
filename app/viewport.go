package app

import (
	"fmt"
	"strings"
	"unicode"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/session"
)

// renderPlaneB renders all ephemeral in-flight content.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	hasPendingTool := m.InFlight.Pending != nil && session.EntryRole(*m.InFlight.Pending) == session.RoleTool
	hasPendingAgent := m.InFlight.Pending != nil && session.EntryRole(*m.InFlight.Pending) == session.RoleAgent
	if !hasPendingTool && len(m.InFlight.PendingTools) == 0 &&
		!hasPendingAgent &&
		m.InFlight.ReasonBuf == "" &&
		len(m.InFlight.Subagents) == 0 {
		return ""
	}

	var b strings.Builder

	// Thinking/reasoning (dimmed, shown while generating)
	if m.InFlight.ReasonBuf != "" {
		b.WriteString(m.planeBLine(m.st.dim, 0, "• Thinking..."))
		b.WriteString("\n")
		if m.verbosity("thinking") == "full" {
			for _, line := range strings.Split(m.InFlight.ReasonBuf, "\n") {
				b.WriteString(m.planeBLine(m.st.dim, 4, line))
				b.WriteString("\n")
			}
		}
	}

	if hasPendingAgent {
		entry := *m.InFlight.Pending
		if content := m.agentStreamContent(); content != "" {
			// content already set on entry
		}
		b.WriteString(m.renderPendingEntry(entry))
		b.WriteString("\n")
	}

	// Active in-flight tools. Sort by ID for deterministic rendering.
	for _, id := range sortedKeys(m.InFlight.PendingTools) {
		b.WriteString(m.renderPendingEntry(m.InFlight.PendingTools[id]))
		b.WriteString("\n")
	}
	if hasPendingTool && len(m.InFlight.PendingTools) == 0 {
		b.WriteString(m.renderPendingEntry(*m.InFlight.Pending))
		b.WriteString("\n")
	}

	// Active subagents
	if n := len(m.InFlight.Subagents); n > 0 {
		// Sort keys for deterministic rendering
		keys := sortedKeys(m.InFlight.Subagents)

		// Show up to 3 active subagent rows
		maxVisible := 3
		shown := 0
		for _, k := range keys {
			if shown >= maxVisible {
				break
			}
			p := m.InFlight.Subagents[k]
			b.WriteString(m.renderSubagentRow(p))
			b.WriteString("\n")
			shown++
		}
		if n > maxVisible {
			b.WriteString(m.planeBLine(m.st.dim, 2, fmt.Sprintf("+%d more workers", n-maxVisible)))
			b.WriteString("\n")
		}
	}

	return b.String()
}

func (m Model) agentStreamContent() string {
	return m.turnReducer().AgentStreamContent()
}

// renderPendingEntry renders an in-flight entry for Plane B.
func (m Model) renderPendingEntry(e session.Entry) string {
	toolVerbosity := m.verbosity("tool")

	switch session.EntryRole(e) {
	case session.RoleAgent:
		if session.EntryContent(e) == "" {
			return m.planeBLine(m.st.dim, 2, "• ...")
		}
		return m.renderLiveAgentContent(session.EntryContent(e))
	case session.RoleTool:
		label := m.normalizeToolTitle(session.EntryTitle(e))
		if label == "" {
			label = "tool"
		}
		var b strings.Builder
		b.WriteString(m.renderToolLabel(label, session.EntryIsError(e)))
		if session.EntryContent(e) == "" || toolVerbosity == "hidden" || m.toolOutputHidden(e) {
			return b.String()
		}
		// When expanded (Ctrl+O), show full output regardless of verbosity
		if !m.ToolOutputExpanded && m.shouldSummarizeToolOutput(e) {
			if isWriteTool(session.EntryTitle(e)) {
				return b.String()
			}
			if summary := toolOutputSummary(e); summary != "" {
				b.WriteString(m.st.dim.Render(" · " + summary))
			}
			return m.planeBFitLine(b.String())
		}
		b.WriteString("\n")
		if !m.ToolOutputExpanded && toolVerbosity == "collapsed" {
			b.WriteString(m.planeBLine(m.st.dim, 4, "..."))
			b.WriteString("\n")
		} else {
			lines := strings.Split(strings.TrimRight(session.EntryContent(e), "\n"), "\n")
			const maxLines = 10
			shown := lines
			if !m.ToolOutputExpanded && len(lines) > maxLines {
				shown = lines[len(lines)-maxLines:]
				b.WriteString(m.planeBLine(m.st.dim, 4, fmt.Sprintf("... (%d lines total)", len(lines))))
				b.WriteString("\n")
			}
			for _, l := range shown {
				b.WriteString(m.planeBLine(m.st.dim, 4, l))
				b.WriteString("\n")
			}
		}
		return b.String()
	case session.RoleSubagent:
		label := session.EntryTitle(e)
		if label == "" {
			label = "subagent"
		}
		var b strings.Builder
		b.WriteString(m.st.subagent.Render("↳ " + label))
		if session.EntryContent(e) != "" {
			b.WriteString("\n")
			b.WriteString(m.planeBLine(m.st.dim, 4, session.EntryContent(e)))
		}
		return b.String()
	default:
		return m.planeBFitLine(session.EntryContent(e))
	}
}

func (m Model) planeBFitLine(line string) string {
	width := m.shellWidth()
	if width <= 0 {
		return line
	}
	return fitLine(line, width)
}

func (m Model) planeBLine(style lipgloss.Style, indent int, text string) string {
	width := m.shellWidth()
	prefix := strings.Repeat(" ", max(0, indent))
	if width <= 0 {
		return style.Render(prefix + text)
	}
	contentWidth := width - ansi.StringWidth(prefix)
	if contentWidth <= 0 {
		return fitLine(style.Render(prefix+text), width)
	}
	return style.Render(prefix + fitLine(text, contentWidth))
}

func (m Model) renderLiveAgentContent(content string) string {
	content = strings.Trim(content, "\n")
	if content == "" {
		return m.st.dim.PaddingLeft(2).Render("• ...")
	}

	width := m.shellWidth()
	if width <= 0 {
		return m.st.agent.Render("• " + content)
	}

	prefix := "• "
	bodyWidth := max(1, width-ansi.StringWidth(prefix))
	var b strings.Builder
	for i, line := range strings.Split(content, "\n") {
		wrapped := ansi.Wordwrap(line, bodyWidth, " \t-")
		if wrapped == "" {
			wrapped = line
		}
		for j, part := range strings.Split(wrapped, "\n") {
			if i > 0 || j > 0 {
				b.WriteString("\n")
			}
			if i == 0 && j == 0 {
				b.WriteString(m.st.agent.Render(prefix + part))
			} else {
				b.WriteString(m.st.agent.Render("  " + part))
			}
		}
	}
	return b.String()
}

func (m Model) renderCompletedAgentContent(rendered string) string {
	lines := m.wrapCompletedAgentLines(rendered)
	if len(lines) == 0 {
		return m.st.agent.Render("• ")
	}

	var b strings.Builder
	for i, line := range lines {
		if i > 0 {
			b.WriteString("\n")
		}
		prefix := "  "
		if i == 0 {
			prefix = "• "
		}
		if line == "" {
			b.WriteString("")
			continue
		}
		b.WriteString(m.st.agent.Render(prefix))
		b.WriteString(line)
	}
	return strings.TrimRightFunc(b.String(), unicode.IsSpace)
}

func (m Model) wrapCompletedAgentLines(rendered string) []string {
	width := m.shellWidth()
	bodyWidth := width - ansi.StringWidth("  ")
	if width <= 0 || bodyWidth <= 0 {
		return strings.Split(rendered, "\n")
	}

	lines := make([]string, 0, strings.Count(rendered, "\n")+1)
	for _, line := range strings.Split(rendered, "\n") {
		if strings.TrimSpace(ansi.Strip(line)) == "" {
			lines = append(lines, "")
			continue
		}
		wrapped := ansi.Wrap(line, bodyWidth, " \t")
		if wrapped == "" {
			lines = append(lines, line)
			continue
		}
		lines = append(lines, strings.Split(wrapped, "\n")...)
	}
	return lines
}

func (m Model) verbosity(kind string) string {
	if m.Model.Config == nil {
		if kind == "thinking" {
			return ""
		}
		return "full"
	}
	switch kind {
	case "tool":
		if v := m.Model.Config.ToolVerbosity; v != "" {
			return v
		}
	case "thinking":
		if m.Model.Config.ThinkingVerbosity == "full" {
			return "full"
		}
		return ""
	}
	return "full"
}

// renderEntry formats a completed entry for the terminal commit boundary.
func (m Model) renderEntry(e session.Entry) string {
	thinkingVerbosity := m.verbosity("thinking")
	toolVerbosity := m.verbosity("tool")

	switch session.EntryRole(e) {
	case session.RoleUser:
		return m.renderUserEntry(session.EntryContent(e))

	case session.RoleAgent:
		var b strings.Builder
		if session.EntryReasoning(e) != "" {
			b.WriteString(m.st.system.Render("• Thinking..."))
			b.WriteString("\n")
			if thinkingVerbosity == "full" {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(session.EntryReasoning(e)))
				b.WriteString("\n")
			}
		}
		rendered := strings.TrimRightFunc(m.renderMarkdownContent(session.EntryContent(e)), unicode.IsSpace)
		if rendered == "" {
			if b.Len() > 0 {
				return strings.TrimRightFunc(b.String(), unicode.IsSpace)
			}
			if session.EntryReasoning(e) != "" {
				b.WriteString(m.st.system.Render("• Thinking..."))
				return strings.TrimRightFunc(b.String(), unicode.IsSpace)
			}
			b.WriteString(m.st.agent.Render("• "))
			return b.String()
		}
		if b.Len() > 0 {
			b.WriteString("\n")
		}
		b.WriteString(m.renderCompletedAgentContent(rendered))
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

	case session.RoleTool:
		label := m.normalizeToolTitle(session.EntryTitle(e))
		if label == "" {
			label = "tool"
		}
		labelStr := m.renderToolLabel(label, session.EntryIsError(e))
		if session.EntryContent(e) == "" || toolVerbosity == "hidden" || m.toolOutputHidden(e) {
			return labelStr
		}
		// When expanded (Ctrl+O), show full output regardless of verbosity
		if !m.ToolOutputExpanded && m.shouldSummarizeToolOutput(e) {
			if isWriteTool(session.EntryTitle(e)) {
				return labelStr
			}
			if summary := toolOutputSummary(e); summary != "" {
				return labelStr + m.st.dim.Render(" · "+summary)
			}
			return labelStr
		}
		content := session.EntryContent(e)
		if m.shouldRenderWriteDiff(e) {
			content = m.renderDiff(content)
		}
		var b strings.Builder
		b.WriteString(labelStr)
		b.WriteString("\n")
		if !m.ToolOutputExpanded && toolVerbosity == "collapsed" {
			b.WriteString(m.st.dim.PaddingLeft(4).Render("..."))
			b.WriteString("\n")
		} else {
			lines := strings.Split(strings.TrimRight(content, "\n"), "\n")
			shown := lines
			if !m.ToolOutputExpanded && len(lines) > 10 {
				shown = lines[:10]
			}
			for _, l := range shown {
				b.WriteString(m.st.dim.Render("  " + l))
				b.WriteString("\n")
			}
			if !m.ToolOutputExpanded && len(lines) > 10 {
				b.WriteString(m.st.dim.Render(
					fmt.Sprintf("  ... (%d more lines)", len(lines)-10),
				))
			}
		}
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

	case session.RoleSubagent:
		label := session.EntryTitle(e)
		if label == "" {
			label = "subagent"
		}
		var b strings.Builder
		b.WriteString(m.st.subagent.Render("↳ " + label))
		if session.EntryContent(e) != "" {
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(session.EntryContent(e)))
		}
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

	case session.RoleSystem:
		if strings.HasPrefix(session.EntryContent(e), "Error: ") {
			return m.st.warn.Render("× " + session.EntryContent(e))
		}
		return m.st.system.Render("• " + session.EntryContent(e))

	default:
		return session.EntryContent(e)
	}
}

func (m Model) renderUserEntry(content string) string {
	content = strings.TrimRight(content, "\n")
	if content == "" {
		return m.st.user.Render("› ")
	}
	rows := strings.Split(content, "\n")
	for i, row := range rows {
		prefix := strings.Repeat(" ", composerPromptWidth())
		if i == 0 {
			prefix = composerPrompt
		}
		rows[i] = m.st.user.Render(prefix + row)
	}
	return strings.Join(rows, "\n")
}

// renderSubagentRow formats a single background worker's status for Plane B.
func (m Model) renderSubagentRow(p *SubagentProgress) string {
	intent := p.Intent
	if ansi.StringWidth(intent) > 24 {
		intent = ansi.Truncate(intent, 24, "...")
	}

	detail := p.Status
	if p.Output != "" {
		lines := strings.Split(strings.TrimSpace(p.Output), "\n")
		if len(lines) > 0 {
			last := strings.TrimSpace(lines[len(lines)-1])
			if last != "" {
				if ansi.StringWidth(last) > 32 {
					last = ansi.Truncate(last, 32, "...")
				}
				detail = fmt.Sprintf("%s: %s", detail, last)
			}
		}
	}

	return m.planeBFitLine(m.st.subagent.Render(fmt.Sprintf("↳ %-10s", p.Name)) + " " +
		m.st.dim.Render(fmt.Sprintf("%-24s", intent)) + " " +
		m.st.dim.Render(detail))
}
