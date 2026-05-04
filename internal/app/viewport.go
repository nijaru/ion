package app

import (
	"fmt"
	"slices"
	"strings"
	"unicode"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/tooldisplay"
)

// Viewport handles the rendering of Plane A (committed scrollback) and Plane B (ephemeral in-flight content).
// Currently, it acts as an extension of the Model receiver, but is planned for componentization.
type Viewport struct{}

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
		b.WriteString(m.st.dim.Render("• Thinking..."))
		b.WriteString("\n")
		thinkingVerbosity := m.verbosity("thinking")
		switch thinkingVerbosity {
		case "full":
			for _, line := range strings.Split(m.InFlight.ReasonBuf, "\n") {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(line))
				b.WriteString("\n")
			}
		default:
			if thinkingVerbosity != "hidden" {
				b.WriteString(m.st.dim.PaddingLeft(4).Render("..."))
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
			b.WriteString(
				m.st.dim.PaddingLeft(2).Render(fmt.Sprintf("+%d more workers", n-maxVisible)),
			)
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
		b.WriteString(m.st.warn.PaddingLeft(2).Render("Approve " + desc + "? (y/n/a)"))
		b.WriteString("\n")
		if environment := strings.TrimSpace(m.Approval.Pending.Environment); environment != "" {
			b.WriteString(
				m.st.dim.PaddingLeft(2).
					Render("Bash env: " + backend.ToolEnvironmentLabel(environment)),
			)
			b.WriteString("\n")
		}
		if summary := escalationSummary(m.Model.Escalation); summary != "" {
			b.WriteString(m.st.dim.PaddingLeft(2).Render("Escalate: " + summary))
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
			return m.st.dim.PaddingLeft(2).Render("• ...")
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
			return b.String()
		}
		b.WriteString("\n")
		if toolVerbosity == "collapsed" {
			b.WriteString(m.st.dim.PaddingLeft(4).Render("..."))
			b.WriteString("\n")
		} else {
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
					fmt.Sprintf("  ... (%d more lines)", len(lines)-10)))
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

func (m Model) renderToolLabel(label string, isError bool) string {
	if isError {
		return m.st.warn.Render("✗") + " " + label
	}
	return m.st.tool.Render("•") + " " + label
}

func (m Model) toolOutputHidden(e session.Entry) bool {
	if e.IsError {
		return false
	}
	switch {
	case isReadLikeTool(e.Title):
		return toolReadOutput(m.Model.Config) == "hidden"
	case isWriteTool(e.Title):
		return toolWriteOutput(m.Model.Config) == "hidden"
	case isBashLikeTool(e.Title):
		return toolBashOutput(m.Model.Config) == "hidden"
	default:
		return false
	}
}

func (m Model) shouldSummarizeToolOutput(e session.Entry) bool {
	if e.Role != session.Tool || e.IsError {
		return false
	}
	if isReadLikeTool(e.Title) {
		return toolReadOutput(m.Model.Config) == "summary"
	}
	if isWriteTool(e.Title) {
		return toolWriteOutput(m.Model.Config) == "summary"
	}
	if isBashLikeTool(e.Title) {
		return toolBashOutput(m.Model.Config) == "summary"
	}
	if m.Model.Config != nil && m.Model.Config.ToolVerbosity == "full" {
		return false
	}
	return isReadLikeTool(e.Title)
}

func (m Model) shouldRenderWriteDiff(e session.Entry) bool {
	return isWriteTool(e.Title) && toolWriteOutput(m.Model.Config) == "diff"
}

func toolReadOutput(cfg *config.Config) string {
	if cfg != nil {
		if output := config.NormalizeReadOutput(cfg.ReadOutput); output != "" {
			return output
		}
		switch cfg.ToolVerbosity {
		case "full":
			return "full"
		case "hidden":
			return "hidden"
		case "collapsed":
			return "summary"
		}
	}
	return "summary"
}

func toolWriteOutput(cfg *config.Config) string {
	if cfg != nil {
		if output := config.NormalizeWriteOutput(cfg.WriteOutput); output != "" {
			return output
		}
		switch cfg.ToolVerbosity {
		case "hidden":
			return "hidden"
		case "collapsed":
			return "summary"
		}
	}
	return "summary"
}

func toolBashOutput(cfg *config.Config) string {
	if cfg != nil {
		if output := config.NormalizeBashOutput(cfg.BashOutput); output != "" {
			return output
		}
		switch cfg.ToolVerbosity {
		case "full":
			return "full"
		case "collapsed":
			return "summary"
		}
	}
	return "hidden"
}

func isReadLikeTool(title string) bool {
	switch toolTitleVerb(title) {
	case "list", "read", "find", "glob", "search", "grep":
		return true
	default:
		return false
	}
}

func isBashLikeTool(title string) bool {
	switch toolTitleVerb(title) {
	case "bash":
		return true
	default:
		return false
	}
}

func toolOutputSummary(e session.Entry) string {
	trimmed := strings.TrimSpace(e.Content)
	if trimmed == "" {
		return ""
	}
	lines := strings.Split(strings.TrimRight(e.Content, "\n"), "\n")
	n := len(lines)
	switch toolTitleVerb(e.Title) {
	case "list", "find", "glob":
		if n == 1 {
			return "1 entry"
		}
		return fmt.Sprintf("%d entries", n)
	case "grep", "search":
		if strings.TrimSpace(e.Content) == "No matches found." {
			return "0 matches"
		}
		if n == 1 {
			return "1 match"
		}
		return fmt.Sprintf("%d matches", n)
	default:
		if n == 1 {
			return "1 line"
		}
		return fmt.Sprintf("%d lines", n)
	}
}

func (m Model) renderQueuedTurns() string {
	if len(m.InFlight.QueuedTurns) == 0 {
		return ""
	}
	preview := compactQueuedText(m.InFlight.QueuedTurns[0])
	label := fmt.Sprintf("• Queued (Ctrl+G edit): %s", preview)
	if extra := len(m.InFlight.QueuedTurns) - 1; extra > 0 {
		label += fmt.Sprintf(" • +%d more", extra)
	}
	return m.st.dim.Render(fitLine(label, m.shellWidth()))
}

func compactQueuedText(text string) string {
	return strings.Join(strings.Fields(strings.TrimSpace(text)), " ")
}

// progressLine renders the single-line progress indicator between Plane B and the composer.
func (m Model) progressLine() string {
	var line string
	if m.Progress.Compacting {
		line = m.Input.Spinner.View() + " Compacting context..."
		if n := len(m.InFlight.QueuedTurns); n > 0 {
			line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
		}
		return fitLine(strings.TrimRight(line, " "), m.shellWidth())
	}
	switch m.Progress.Mode {
	case stateIonizing, stateStreaming, stateWorking:
		status := m.Progress.Status
		if isIdleStatus(status) || isConfigurationStatus(status) {
			switch m.Progress.Mode {
			case stateIonizing:
				status = "Ionizing..."
			case stateStreaming:
				status = "Streaming..."
			case stateWorking:
				if len(m.InFlight.Subagents) > 0 {
					// Prefer showing wait status for subagents
					for _, k := range sortedKeys(m.InFlight.Subagents) {
						status = "Waiting for " + m.InFlight.Subagents[k].Name + "..."
						break
					}
				} else {
					status = "Working..."
				}
			}
		}
		line = m.Input.Spinner.View() + " " + status
		if stats := m.runningProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case stateComplete:
		line = m.st.success.Render("✓") + " Complete"
		if stats := m.completedProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case stateApproval:
		line = m.st.warn.Render("⚠ Approval required")
	case stateCancelled:
		line = m.st.warn.Render("⚠ Canceled")
		if reason := strings.TrimSpace(m.Progress.BudgetStopReason); reason != "" {
			line += " • " + reason
		}
	case stateBlocked:
		line = m.st.warn.Render("⚠ Subagent blocked")
	case stateError:
		line = m.st.warn.Render("× Error")
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
	return fitLine(strings.TrimRight(line, " "), m.shellWidth())
}

func (m Model) renderProgressStats(parts []string) string {
	if len(parts) == 0 {
		return ""
	}
	var b strings.Builder
	for _, part := range parts {
		b.WriteString(m.st.dim.Render(" • "))
		b.WriteString(m.st.dim.Render(part))
	}
	return b.String()
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
	lower := toolTitleVerb(title)
	for _, prefix := range []string{"write", "edit", "create"} {
		if lower == prefix {
			return true
		}
	}
	return false
}

func toolTitleVerb(title string) string {
	title = strings.TrimSpace(strings.ToLower(title))
	if title == "" {
		return ""
	}
	if idx := strings.IndexAny(title, " ("); idx >= 0 {
		return strings.TrimSpace(title[:idx])
	}
	return title
}

func (m Model) normalizeToolTitle(title string) string {
	return tooldisplay.NormalizeTitle(title, m.toolTitleOptions())
}

// FormatToolTitle attempts to extract the most important argument from a tool call's
// raw JSON string to create a more readable title.
func FormatToolTitle(name, args string) string {
	return tooldisplay.Title(name, args, tooldisplay.Options{})
}

func (m Model) formatToolTitle(name, args string) string {
	return tooldisplay.Title(name, args, m.toolTitleOptions())
}

func (m Model) toolTitleOptions() tooldisplay.Options {
	width := 0
	if m.shellWidth() > 0 {
		width = max(0, m.shellWidth()-2)
	}
	return tooldisplay.Options{
		Workdir: m.App.Workdir,
		Width:   width,
	}
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

// renderSubagentRow formats a single background worker's status for Plane B.
func (m Model) renderSubagentRow(p *SubagentProgress) string {
	intent := p.Intent
	if len(intent) > 24 {
		intent = intent[:21] + "..."
	}

	detail := p.Status
	if p.Output != "" {
		lines := strings.Split(strings.TrimSpace(p.Output), "\n")
		if len(lines) > 0 {
			last := strings.TrimSpace(lines[len(lines)-1])
			if last != "" {
				if len(last) > 32 {
					last = last[:29] + "..."
				}
				detail = fmt.Sprintf("%s: %s", detail, last)
			}
		}
	}

	return m.st.subagent.Render(fmt.Sprintf("↳ %-10s", p.Name)) + " " +
		m.st.dim.Render(fmt.Sprintf("%-24s", intent)) + " " +
		m.st.dim.Render(detail)
}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	return keys
}
