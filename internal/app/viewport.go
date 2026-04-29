package app

import (
	"fmt"
	"slices"
	"strings"
	"unicode"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

// Viewport handles the rendering of Plane A (committed scrollback) and Plane B (ephemeral in-flight content).
// Currently, it acts as an extension of the Model receiver, but is planned for componentization.
type Viewport struct{}

// renderPlaneB renders all ephemeral in-flight content.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	hasPendingTool := m.InFlight.Pending != nil && m.InFlight.Pending.Role == session.Tool
	if !hasPendingTool && len(m.InFlight.PendingTools) == 0 && m.Approval.Pending == nil && m.InFlight.ReasonBuf == "" && len(m.InFlight.Subagents) == 0 {
		return ""
	}

	var b strings.Builder

	// Thinking/reasoning (dimmed, shown while generating)
	if m.InFlight.ReasonBuf != "" {
		b.WriteString(m.st.dim.Render("• Thinking..."))
		b.WriteString("\n")
		switch m.verbosity("thinking") {
		case "full":
			for _, line := range strings.Split(m.InFlight.ReasonBuf, "\n") {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(line))
				b.WriteString("\n")
			}
		case "hidden":
			return b.String()
		default:
			b.WriteString(m.st.dim.PaddingLeft(4).Render("..."))
			b.WriteString("\n")
		}
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
			b.WriteString(m.st.dim.PaddingLeft(2).Render(fmt.Sprintf("+%d more workers", n-maxVisible)))
			b.WriteString("\n")
		}
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
		return m.st.agent.Render("• " + e.Content)
	case session.Tool:
		label := e.Title
		if label == "" {
			label = "tool"
		}
		var b strings.Builder
		b.WriteString(m.st.tool.Render("• " + label))
		if e.Content == "" || toolVerbosity == "hidden" {
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

func (m Model) verbosity(kind string) string {
	if m.Model.Config == nil {
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
		return "collapsed"
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
			b.WriteString(m.st.system.Render("• Thinking"))
			b.WriteString("\n")
			if thinkingVerbosity == "collapsed" {
				b.WriteString(m.st.dim.PaddingLeft(4).Render("..."))
				b.WriteString("\n")
			} else {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Reasoning))
				b.WriteString("\n")
			}
		}
		rendered := strings.TrimRightFunc(m.renderMarkdownContent(e.Content), unicode.IsSpace)
		if rendered == "" {
			b.WriteString(m.st.agent.Render("• "))
			return b.String()
		}
		b.WriteString(m.st.agent.Render("• "))
		b.WriteString(rendered)
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
		if e.Content == "" || toolVerbosity == "hidden" {
			return labelStr
		}
		content := e.Content
		if shouldCompactRoutineTool(e, m.Model.Config) {
			content = summarizeRoutineToolOutput(content)
		}
		if isWriteTool(e.Title) {
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

func shouldCompactRoutineTool(e session.Entry, cfg *config.Config) bool {
	if e.Role != session.Tool || e.IsError {
		return false
	}
	if cfg != nil && cfg.ToolVerbosity == "full" {
		return false
	}
	switch toolTitleVerb(e.Title) {
	case "list", "read", "find", "glob", "search", "grep":
		return true
	default:
		return false
	}
}

func summarizeRoutineToolOutput(content string) string {
	trimmed := strings.TrimSpace(content)
	if strings.HasPrefix(trimmed, "... (") && strings.HasSuffix(trimmed, ")") {
		return trimmed
	}
	lines := strings.Split(strings.TrimRight(content, "\n"), "\n")
	if len(lines) == 1 {
		if strings.TrimSpace(lines[0]) == "" {
			return ""
		}
		return "... (1 line)"
	}
	return fmt.Sprintf("... (%d lines)", len(lines))
}

// progressLine renders the single-line progress indicator between Plane B and the composer.
func (m Model) progressLine() string {
	var line string
	if m.Progress.Compacting {
		line = m.Input.Spinner.View() + " Compacting context..."
		if n := len(m.InFlight.QueuedTurns); n > 0 {
			line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
		}
		return fitLine(strings.TrimRight(line, " "), m.App.Width)
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
		if reason := strings.TrimSpace(m.Progress.BudgetStopReason); reason != "" {
			line += " • " + reason
		}
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

// FormatToolTitle attempts to extract the most important argument from a tool call's
// raw JSON string to create a more readable title.
func FormatToolTitle(name, args string) string {
	args = strings.TrimSpace(args)
	displayName := toolDisplayName(name)
	if args == "" || args == "{}" {
		return displayName
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
					return fmt.Sprintf("%s %s", displayName, val[:end])
				}
			}
		}
	}

	if !strings.HasPrefix(args, "{") && !strings.HasPrefix(args, "[") {
		return fmt.Sprintf("%s %s", displayName, args)
	}
	if strings.Contains(args, "[redacted-secret]") {
		return fmt.Sprintf("%s %s", displayName, args)
	}

	return displayName
}

func toolDisplayName(name string) string {
	switch strings.TrimSpace(strings.ToLower(name)) {
	case "read":
		return "Read"
	case "write":
		return "Write"
	case "edit":
		return "Edit"
	case "multi_edit":
		return "Edit"
	case "list":
		return "List"
	case "grep":
		return "Search"
	case "glob":
		return "Find"
	case "bash":
		return "Bash"
	case "verify":
		return "Verify"
	default:
		if strings.TrimSpace(name) == "" {
			return "Tool"
		}
		return name
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
