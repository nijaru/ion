package app

import (
	"fmt"
	"strings"

	"github.com/alecthomas/chroma/v2/formatters"
	"github.com/alecthomas/chroma/v2/lexers"
	chromastyles "github.com/alecthomas/chroma/v2/styles"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

func highlightSyntax(code, language string) string {
	// Get lexer for the language
	lexer := lexers.Get(language)
	if lexer == nil {
		lexer = lexers.Analyse(code)
	}
	if lexer == nil {
		lexer = lexers.Fallback
	}

	// Use catppuccin-mocha (dark theme)
	style := chromastyles.Get("catppuccin-mocha")
	if style == nil {
		style = chromastyles.Fallback
	}

	// Use terminal formatter (ANSI escape codes)
	formatter := formatters.Get("terminal")
	if formatter == nil {
		formatter = formatters.Fallback
	}

	// Tokenize and format
	iterator, err := lexer.Tokenise(nil, code)
	if err != nil {
		return code
	}

	var buf strings.Builder
	err = formatter.Format(&buf, style, iterator)
	if err != nil {
		return code
	}

	return buf.String()
}

// highlightCodeBlock applies syntax highlighting to a code block,
// preserving indentation. Each line is highlighted individually to
// maintain consistent styling across the block.
func (m Model) renderToolLabel(label string, isError bool) string {
	if isError {
		return m.st.warn.Render("✗") + " " + label
	}
	return m.st.tool.Render("•") + " " + label
}

func (m Model) toolOutputHidden(e session.Entry) bool {
	if session.EntryIsError(e) {
		return false
	}
	switch {
	case isReadLikeTool(session.EntryTitle(e)):
		return toolReadOutput(m.Model.Config) == "hidden"
	case isWriteTool(session.EntryTitle(e)):
		return toolWriteOutput(m.Model.Config) == "hidden"
	case isBashLikeTool(session.EntryTitle(e)):
		return toolBashOutput(m.Model.Config) == "hidden"
	default:
		return false
	}
}

func (m Model) shouldSummarizeToolOutput(e session.Entry) bool {
	if session.EntryRole(e) != session.RoleTool || session.EntryIsError(e) {
		return false
	}
	if isReadLikeTool(session.EntryTitle(e)) {
		return toolReadOutput(m.Model.Config) == "summary"
	}
	if isWriteTool(session.EntryTitle(e)) {
		return toolWriteOutput(m.Model.Config) == "summary"
	}
	if isBashLikeTool(session.EntryTitle(e)) {
		return toolBashOutput(m.Model.Config) == "summary"
	}
	if m.Model.Config != nil && m.Model.Config.ToolVerbosity == "full" {
		return false
	}
	return isReadLikeTool(session.EntryTitle(e))
}

func (m Model) shouldRenderWriteDiff(e session.Entry) bool {
	return isWriteTool(session.EntryTitle(e)) && toolWriteOutput(m.Model.Config) == "diff"
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
	case "list", "ls", "read", "find", "glob", "search", "grep":
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
	trimmed := strings.TrimSpace(session.EntryContent(e))
	if trimmed == "" {
		return ""
	}
	lines := strings.Split(strings.TrimRight(session.EntryContent(e), "\n"), "\n")
	n := len(lines)
	switch toolTitleVerb(session.EntryTitle(e)) {
	case "list", "ls", "find", "glob":
		if n == 1 {
			return "1 entry"
		}
		return fmt.Sprintf("%d entries", n)
	case "grep", "search":
		if strings.TrimSuffix(strings.TrimSpace(session.EntryContent(e)), ".") == "No matches found" {
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
	return tool.NormalizeTitle(title, m.toolTitleOptions())
}

// FormatToolTitle attempts to extract the most important argument from a tool call's
// raw JSON string to create a more readable title.
func FormatToolTitle(name, args string) string {
	return tool.Title(name, args, tool.Options{})
}

func (m Model) formatToolTitle(name, args string) string {
	return tool.Title(name, args, tool.Options{Workdir: m.App.Workdir})
}

func (m Model) toolTitleOptions() tool.Options {
	width := 0
	if m.shellWidth() > 0 {
		width = max(0, m.shellWidth()-2)
	}
	return tool.Options{
		Workdir: m.App.Workdir,
		Width:   width,
	}
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
