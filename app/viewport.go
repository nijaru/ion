package app

import (
	"fmt"
	"strings"
	"unicode"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/terminal"
	"github.com/nijaru/ion/session"
)

// renderPlaneB renders all ephemeral in-flight content.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	hasPendingTool := m.InFlight.Pending != nil && session.EntryRole(*m.InFlight.Pending) == session.RoleTool
	hasPendingAgent := m.InFlight.Pending != nil && session.EntryRole(*m.InFlight.Pending) == session.RoleAgent
	if !hasPendingTool && len(m.InFlight.PendingTools) == 0 &&
		!hasPendingAgent &&
		m.InFlight.ReasonBuf == "" {
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
		// Tool-call-only assistant messages are represented by the active tool
		// entries below. Rendering the assistant shell as well duplicates labels
		// and leaves a bare bullet when the tool loop finishes.
		if rendered := m.renderPendingEntry(entry); rendered != "" {
			b.WriteString(rendered)
			b.WriteString("\n")
		}
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

	return b.String()
}

func (m Model) agentStreamContent() string {
	return m.turnReducer().AgentStreamContent()
}

func pendingToolCalls(e session.Entry) []*session.ToolCall {
	me, ok := e.(*session.MessageEntry)
	if !ok {
		return nil
	}
	am, ok := me.Message.(*session.AssistantMessage)
	if !ok {
		return nil
	}
	calls := make([]*session.ToolCall, 0, len(am.Content))
	for _, content := range am.Content {
		if call, ok := content.(*session.ToolCall); ok && call != nil {
			calls = append(calls, call)
		}
	}
	return calls
}

// renderPendingEntry renders an in-flight entry for Plane B.
func (m Model) renderPendingEntry(e session.Entry) string {
	toolVerbosity := m.verbosity("tool")

	switch session.EntryRole(e) {
	case session.RoleAgent:
		// An assistant shell can legitimately contain only tool calls or be
		// waiting for its first text delta. Keep that shell retained in state,
		// but do not project an orphan bullet into the transcript.
		if strings.TrimSpace(session.EntryContent(e)) == "" || len(pendingToolCalls(e)) > 0 {
			return ""
		}
		return m.renderLiveAgentContent(session.EntryContent(e))
	case session.RoleTool:
		label := m.normalizeToolTitle(session.EntryTitle(e))
		if label == "" {
			label = "tool"
		}
		if isSubagentTool(session.EntryTitle(e)) {
			var b strings.Builder
			b.WriteString(m.st.subagent.Render("↳ " + label))
			if session.EntryContent(e) != "" {
				b.WriteString("\n")
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

	// Use the same Markdown projection for the mutable stream and the terminal
	// commit. Promotion must not change headings, lists, code blocks, or
	// paragraph spacing when the final assistant message arrives.
	rendered := strings.TrimRightFunc(m.renderMarkdownContent(content), unicode.IsSpace)
	if rendered == "" {
		return m.st.dim.PaddingLeft(2).Render("• ...")
	}
	return m.renderCompletedAgentContent(rendered)
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
		text := m.renderUserEntry(session.EntryContent(e))
		if images := session.EntryImages(e); len(images) > 0 {
			var b strings.Builder
			b.WriteString(text)
			for _, img := range images {
				b.WriteString("\n")
				inlineImg := terminal.RenderInlineImage(img.MimeType, img.Data, max(20, min(80, m.shellWidth()-4)))
				b.WriteString(m.st.dim.PaddingLeft(2).Render(inlineImg))
			}
			return b.String()
		}
		return text

	case session.RoleAgent:
		if strings.TrimSpace(session.EntryContent(e)) == "" && strings.TrimSpace(session.EntryReasoning(e)) == "" {
			return ""
		}
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
				return terminal.WrapTurnOutput(strings.TrimRightFunc(b.String(), unicode.IsSpace), false)
			}
			if session.EntryReasoning(e) != "" {
				b.WriteString(m.st.system.Render("• Thinking..."))
				return terminal.WrapTurnOutput(strings.TrimRightFunc(b.String(), unicode.IsSpace), false)
			}
			b.WriteString(m.st.agent.Render("• "))
			return terminal.WrapTurnOutput(b.String(), false)
		}
		if b.Len() > 0 {
			b.WriteString("\n")
		}
		b.WriteString(m.renderCompletedAgentContent(rendered))
		return terminal.WrapTurnOutput(strings.TrimRightFunc(b.String(), unicode.IsSpace), false)

	case session.RoleTool:
		label := m.normalizeToolTitle(session.EntryTitle(e))
		if label == "" {
			label = "tool"
		}
		labelStr := m.renderToolLabel(label, session.EntryIsError(e))
		if isSubagentTool(session.EntryTitle(e)) {
			labelStr = m.st.subagent.Render("↳ " + label)
		}
		if session.EntryContent(e) == "" || toolVerbosity == "hidden" || m.toolOutputHidden(e) {
			return labelStr
		}
		// When expanded (Ctrl+O), show full output regardless of verbosity
		images := session.EntryImages(e)
		if !m.ToolOutputExpanded && m.shouldSummarizeToolOutput(e) {
			if len(images) == 0 {
				if isWriteTool(session.EntryTitle(e)) {
					return labelStr
				}
				if summary := toolOutputSummary(e); summary != "" {
					return labelStr + m.st.dim.Render(" · "+summary)
				}
				return labelStr
			}
			var b strings.Builder
			b.WriteString(labelStr)
			if summary := toolOutputSummary(e); summary != "" {
				b.WriteString(m.st.dim.Render(" · " + summary))
			}
			for _, img := range images {
				b.WriteString("\n")
				inlineImg := terminal.RenderInlineImage(img.MimeType, img.Data, max(20, min(80, m.shellWidth()-4)))
				b.WriteString(m.st.dim.PaddingLeft(2).Render(inlineImg))
			}
			return strings.TrimRightFunc(b.String(), unicode.IsSpace)
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
				b.WriteString("\n")
			}
		}
		if images := session.EntryImages(e); len(images) > 0 {
			for _, img := range images {
				inlineImg := terminal.RenderInlineImage(img.MimeType, img.Data, max(20, min(80, m.shellWidth()-4)))
				if inlineImg != "" {
					b.WriteString(m.st.dim.PaddingLeft(2).Render(inlineImg))
					b.WriteString("\n")
				}
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
		return terminal.WrapUserPrompt(m.st.user.Render("› "))
	}
	rows := strings.Split(content, "\n")
	for i, row := range rows {
		prefix := strings.Repeat(" ", composerPromptWidth())
		if i == 0 {
			prefix = composerPrompt
		}
		rows[i] = m.st.user.Render(prefix + row)
	}
	return terminal.WrapUserPrompt(strings.Join(rows, "\n"))
}
