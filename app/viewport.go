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

type liveTranscriptNodeKind uint8

const (
	liveTranscriptThinking liveTranscriptNodeKind = iota
	liveTranscriptEntry
)

// liveTranscriptNode is the retained render projection for one ephemeral turn
// component. Its key is stable across view redraws, while the entry payload is
// replaced by the runtime event reducer as content arrives.
type liveTranscriptNode struct {
	key   string
	kind  liveTranscriptNodeKind
	text  string
	entry session.Entry
}

func (m Model) liveTranscriptNodes() []liveTranscriptNode {
	nodes := make([]liveTranscriptNode, 0, len(m.InFlight.PendingTools)+len(m.InFlight.CompletedTools)+3)
	if m.InFlight.Submitted != nil {
		nodes = append(nodes, liveTranscriptNode{
			key:   "user",
			kind:  liveTranscriptEntry,
			entry: *m.InFlight.Submitted,
		})
	}
	if m.InFlight.ReasonBuf != "" {
		nodes = append(nodes, liveTranscriptNode{
			key:  "thinking",
			kind: liveTranscriptThinking,
			text: m.InFlight.ReasonBuf,
		})
	}

	// The assistant narrative belongs before the tool batch it introduces.
	// Tool-only assistant shells remain invisible, while mixed text and tool
	// calls retain their narrative in the same turn position.
	if m.InFlight.Pending != nil {
		entry := *m.InFlight.Pending
		if session.EntryRole(entry) == session.RoleAgent && m.renderPendingEntry(entry) != "" {
			nodes = append(nodes, liveTranscriptNode{
				key:   "assistant",
				kind:  liveTranscriptEntry,
				entry: entry,
			})
		}
	}

	completed := make(map[string]session.Entry, len(m.InFlight.CompletedTools))
	for index, entry := range m.InFlight.CompletedTools {
		id := toolEntryID(entry)
		if id == "" {
			id = fmt.Sprintf("completed:%d", index)
		}
		completed[id] = entry
	}
	seen := make(map[string]struct{}, len(completed)+len(m.InFlight.PendingTools))
	appendTool := func(id string, entry session.Entry) {
		if entry == nil {
			return
		}
		if _, exists := seen[id]; exists {
			return
		}
		seen[id] = struct{}{}
		nodes = append(nodes, liveTranscriptNode{
			key:   "tool:" + id,
			kind:  liveTranscriptEntry,
			entry: entry,
		})
	}
	for _, id := range m.InFlight.ToolOrder {
		if entry, ok := completed[id]; ok {
			appendTool(id, entry)
			continue
		}
		if entry, ok := m.InFlight.PendingTools[id]; ok {
			appendTool(id, entry)
		}
	}
	for index, entry := range m.InFlight.CompletedTools {
		id := toolEntryID(entry)
		if id == "" {
			id = fmt.Sprintf("completed:%d", index)
		}
		appendTool(id, entry)
	}
	for _, id := range sortedKeys(m.InFlight.PendingTools) {
		appendTool(id, m.InFlight.PendingTools[id])
	}
	if m.InFlight.Pending != nil && session.EntryRole(*m.InFlight.Pending) == session.RoleTool &&
		len(m.InFlight.PendingTools) == 0 && m.renderPendingEntry(*m.InFlight.Pending) != "" {
		nodes = append(nodes, liveTranscriptNode{
			key:   "tool:pending",
			kind:  liveTranscriptEntry,
			entry: *m.InFlight.Pending,
		})
	}
	return nodes
}

func toolEntryID(entry session.Entry) string {
	messageEntry, ok := entry.(*session.MessageEntry)
	if !ok {
		return ""
	}
	result, ok := messageEntry.Message.(*session.ToolResultMessage)
	if !ok {
		return ""
	}
	return strings.TrimSpace(result.ToolCallID)
}

// renderPlaneB renders the retained ephemeral turn projection. Completed
// scrollback is owned by terminalCommit; this view contains only the current
// node set and can therefore be redrawn without rewriting history.
func (m Model) renderPlaneB() string {
	nodes := m.liveTranscriptNodes()
	if len(nodes) == 0 {
		return ""
	}

	var b strings.Builder
	for _, node := range nodes {
		if node.kind == liveTranscriptThinking {
			b.WriteString(m.planeBLine(m.st.dim, 0, "• Thinking..."))
			b.WriteString("\n")
			if m.verbosity("thinking") == "full" {
				for _, line := range strings.Split(node.text, "\n") {
					b.WriteString(m.planeBLine(m.st.dim, 4, line))
					b.WriteString("\n")
				}
			}
			continue
		}
		if rendered := m.renderPendingEntry(node.entry); rendered != "" {
			b.WriteString(rendered)
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
		// An assistant shell can legitimately contain only tool calls or be
		// waiting for its first text delta. Keep that shell retained in state,
		// but do not project an orphan bullet into the transcript. Narrative
		// text must remain visible even when the same message also calls a tool.
		if strings.TrimSpace(session.EntryContent(e)) == "" {
			return ""
		}
		return m.renderLiveAgentContent(session.EntryContent(e))
	case session.RoleUser:
		return m.renderUserEntry(session.EntryContent(e))
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
