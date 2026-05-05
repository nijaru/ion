package app

import (
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/tooldisplay"
)

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
