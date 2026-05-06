package app

import (
	"fmt"
	"strings"
	"unicode"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
)

// renderPlaneB renders all ephemeral in-flight content.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	hasPendingTool := m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Tool
	hasPendingAgent := m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Agent
	if !hasPendingTool && len(m.InFlight.PendingTools) == 0 && m.Approval.Pending == nil &&
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
		thinkingVerbosity := m.verbosity("thinking")
		switch thinkingVerbosity {
		case "full":
			for _, line := range strings.Split(m.InFlight.ReasonBuf, "\n") {
				b.WriteString(m.planeBLine(m.st.dim, 4, line))
				b.WriteString("\n")
			}
		default:
			if thinkingVerbosity != "hidden" {
				b.WriteString(m.planeBLine(m.st.dim, 4, "..."))
				b.WriteString("\n")
			}
		}
	}

	if hasPendingAgent {
		b.WriteString(m.renderPendingEntry(*m.InFlight.Pending))
		b.WriteString("\n")
	}

	// Active in-flight tools. Sort by ID for deterministic rendering.
	for _, id := range sortedKeys(m.InFlight.PendingTools) {
		b.WriteString(m.renderPendingEntry(*m.InFlight.PendingTools[id]))
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

	// Approval prompt
	if m.Approval.Pending != nil {
		b.WriteString("\n")
		desc := m.Approval.Pending.Description
		if m.Approval.Pending.ToolName != "" {
			desc = fmt.Sprintf("%s: %s",
				m.formatToolTitle(m.Approval.Pending.ToolName, m.Approval.Pending.Args),
				m.Approval.Pending.Description)
		}
		b.WriteString(m.planeBLine(m.st.warn, 2, "Approve "+desc+"? (y/n/a)"))
		b.WriteString("\n")
		if environment := strings.TrimSpace(m.Approval.Pending.Environment); environment != "" {
			b.WriteString(
				m.planeBLine(m.st.dim, 2, "Bash env: "+backend.ToolEnvironmentLabel(environment)),
			)
			b.WriteString("\n")
		}
		if summary := escalationSummary(m.Model.Escalation); summary != "" {
			b.WriteString(m.planeBLine(m.st.dim, 2, "Escalate: "+summary))
			b.WriteString("\n")
		}
	}

	return b.String()
}

// renderPendingEntry renders an in-flight entry for Plane B.
func (m Model) renderPendingEntry(e session.Entry) string {
	toolVerbosity := m.verbosity("tool")

	switch e.Role {
	case session.Agent:
		if e.Content == "" {
			return m.planeBLine(m.st.dim, 2, "• ...")
		}
		return m.renderLiveAgentContent(e.Content)
	case session.Tool:
		label := m.normalizeToolTitle(e.Title)
		if label == "" {
			label = "tool"
		}
		var b strings.Builder
		b.WriteString(m.renderToolLabel(label, e.IsError))
		if e.Content == "" || toolVerbosity == "hidden" || m.toolOutputHidden(e) {
			return b.String()
		}
		if m.shouldSummarizeToolOutput(e) {
			if isWriteTool(e.Title) {
				return b.String()
			}
			if summary := toolOutputSummary(e); summary != "" {
				b.WriteString(m.st.dim.Render(" · " + summary))
			}
			return m.planeBFitLine(b.String())
		}
		b.WriteString("\n")
		if toolVerbosity == "collapsed" {
			b.WriteString(m.planeBLine(m.st.dim, 4, "..."))
			b.WriteString("\n")
		} else {
			lines := strings.Split(strings.TrimRight(e.Content, "\n"), "\n")
			const maxLines = 10
			shown := lines
			if len(lines) > maxLines {
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
	case session.Subagent:
		label := e.Title
		if label == "" {
			label = "subagent"
		}
		var b strings.Builder
		b.WriteString(m.st.subagent.Render("↳ " + label))
		if e.Content != "" {
			b.WriteString("\n")
			b.WriteString(m.planeBLine(m.st.dim, 4, e.Content))
		}
		return b.String()
	default:
		return m.planeBFitLine(e.Content)
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

func (m Model) verbosity(kind string) string {
	if m.Model.Config == nil {
		if kind == "thinking" {
			return "hidden"
		}
		return "full"
	}
	switch kind {
	case "tool":
		if v := m.Model.Config.ToolVerbosity; v != "" {
			return v
		}
	case "thinking":
		if v := m.Model.Config.ThinkingVerbosity; v != "" {
			return v
		}
		return "hidden"
	}
	return "full"
}

// renderEntry formats a completed entry for tea.Printf scrollback commit.
func (m Model) renderEntry(e session.Entry) string {
	thinkingVerbosity := m.verbosity("thinking")
	toolVerbosity := m.verbosity("tool")

	switch e.Role {
	case session.User:
		return m.st.user.Render("› " + e.Content)

	case session.Agent:
		var b strings.Builder
		if e.Reasoning != "" && thinkingVerbosity != "hidden" {
			if thinkingVerbosity == "collapsed" {
				b.WriteString(m.st.system.Render("• Thinking..."))
				b.WriteString("\n")
			} else {
				b.WriteString(m.st.system.Render("• Thinking"))
				b.WriteString("\n")
				b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Reasoning))
				b.WriteString("\n")
			}
		}
		rendered := strings.TrimRightFunc(m.renderMarkdownContent(e.Content), unicode.IsSpace)
		if rendered == "" {
			if b.Len() > 0 {
				return strings.TrimRightFunc(b.String(), unicode.IsSpace)
			}
			if e.Reasoning != "" {
				b.WriteString(m.st.system.Render("• Thinking"))
				return strings.TrimRightFunc(b.String(), unicode.IsSpace)
			}
			b.WriteString(m.st.agent.Render("• "))
			return b.String()
		}
		b.WriteString(m.st.agent.Render("• "))
		b.WriteString(rendered)
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

	case session.Tool:
		label := m.normalizeToolTitle(e.Title)
		if label == "" {
			label = "tool"
		}
		labelStr := m.renderToolLabel(label, e.IsError)
		if e.Content == "" || toolVerbosity == "hidden" || m.toolOutputHidden(e) {
			return labelStr
		}
		if m.shouldSummarizeToolOutput(e) {
			if isWriteTool(e.Title) {
				return labelStr
			}
			if summary := toolOutputSummary(e); summary != "" {
				return labelStr + m.st.dim.Render(" · "+summary)
			}
			return labelStr
		}
		content := e.Content
		if m.shouldRenderWriteDiff(e) {
			content = m.renderDiff(content)
		}
		var b strings.Builder
		b.WriteString(labelStr)
		b.WriteString("\n")
		if toolVerbosity == "collapsed" {
			b.WriteString(m.st.dim.PaddingLeft(4).Render("..."))
			b.WriteString("\n")
		} else {
			lines := strings.Split(strings.TrimRight(content, "\n"), "\n")
			shown := lines
			if len(lines) > 10 {
				shown = lines[:10]
			}
			for _, l := range shown {
				b.WriteString(m.st.dim.Render("  " + l))
				b.WriteString("\n")
			}
			if len(lines) > 10 {
				b.WriteString(m.st.dim.Render(
					fmt.Sprintf("  ... (%d more lines)", len(lines)-10),
				))
			}
		}
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

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
		return strings.TrimRightFunc(b.String(), unicode.IsSpace)

	case session.System:
		if strings.HasPrefix(e.Content, "Error: ") {
			return m.st.warn.Render("× " + e.Content)
		}
		return m.st.system.Render("• " + e.Content)

	default:
		return e.Content
	}
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
