package app

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func (m Model) statusLine() string {
	width := m.shellWidth()
	contentWidth := width - 1
	if contentWidth <= 0 {
		return insetStatusLine("", width)
	}
	if hint := strings.TrimSpace(m.pendingActionStatus()); hint != "" {
		return insetStatusLine(m.st.warn.Render(hint), width)
	}

	sep := m.st.sep.Render(" • ")

	provider := ""
	model := ""
	limit := 0
	if m.Model.Backend != nil {
		if value := m.Model.Backend.Provider(); value != "" {
			provider = m.st.dim.Render(value)
		}
		if value := m.Model.Backend.Model(); value != "" {
			model = m.st.dim.Render(m.statusModelLabel(value))
		}
		limit = m.Model.Backend.ContextLimit()
	}
	thinking := m.st.dim.Render(normalizeThinkingValue(m.Progress.ReasoningEffort))
	dir := m.st.dim.Render(statusWorkdirLabel(m.App.Workdir))
	branch := ""
	if m.App.Branch != "" {
		branch = m.st.dim.Render(m.App.Branch)
	}
	gitDiff := ""
	if value := strings.TrimSpace(m.App.GitDiff); value != "" {
		gitDiff = m.st.dim.Render(value)
	}

	usage := m.renderContextUsage(m.Progress.ContextTokens, limit)

	cost := ""
	if label := m.costBudgetLabel(m.Progress.TotalCost); label != "" {
		cost = m.st.dim.Render(label)
	}

	candidates := [][]string{
		{
			provider,
			model,
			thinking,
			usage,
			cost,
			dir,
			branch,
			gitDiff,
		},
		{provider, model, thinking, usage, cost, gitDiff},
		{provider, model, thinking, usage, cost},
		{model, thinking, usage, cost},
		{thinking, usage, cost},
	}
	for _, segments := range candidates {
		line := joinLineSegments(sep, segments...)
		if ansi.StringWidth(line) <= contentWidth {
			return insetStatusLine(line, width)
		}
	}

	return insetStatusLine(joinLineSegments(sep, thinking, usage, cost), width)
}

func insetStatusLine(line string, width int) string {
	if width <= 0 {
		return ""
	}
	return " " + fitLine(line, width-1)
}

func (m Model) statusModelLabel(model string) string {
	if m.activePreset() != presetFast {
		return model
	}
	return model + " (fast)"
}

func (m Model) renderContextUsage(total, limit int) string {
	if total <= 0 {
		return ""
	}
	if limit <= 0 {
		return m.st.dim.Render(fmt.Sprintf("%s tokens", compactCount(total)))
	}

	pct := (total * 100) / limit
	label := fmt.Sprintf("%s/%s (%d%%)", compactCount(total), compactCount(limit), pct)
	switch {
	case pct >= 80:
		return m.st.warn.Render(label)
	case pct >= 50:
		return m.st.caution.Render(label)
	default:
		return m.st.success.Render(label)
	}
}

func statusWorkdirLabel(workdir string) string {
	if strings.TrimSpace(workdir) == "" {
		return ""
	}
	label := filepath.Base(filepath.Clean(workdir))
	if label == string(os.PathSeparator) {
		return label
	}
	return label + string(os.PathSeparator)
}

func (m *Model) layout() {
	width := m.shellWidth()
	if width <= 0 {
		width = 1
	}
	m.Input.Composer.SetWidth(width)
}

func (m Model) handleWindowSize(msg tea.WindowSizeMsg) (Model, tea.Cmd) {
	oldWidth := m.App.Width
	m.App.Ready = true
	m.App.Width = msg.Width
	m.App.Height = msg.Height
	m.layout()
	if oldWidth > 0 && msg.Width > 0 && msg.Width < oldWidth {
		return m, clearVisibleScreenCmd()
	}
	return m, nil
}

func (m Model) headerLine() string {
	return m.headerLineFor(m.App.Branch)
}

func (m Model) headerLineFor(branch string) string {
	sep := m.st.dim.Render(" • ")

	home, _ := os.UserHomeDir()
	dir := shortenHomePath(m.App.Workdir, home)

	pathParts := []string{m.st.dim.Render(dir)}
	if branch != "" {
		pathParts = append(pathParts, m.st.dim.Render(branch))
	}
	return strings.Join(pathParts, sep)
}

func shortenHomePath(path, home string) string {
	if home == "" || path == "" {
		return path
	}
	if path == home {
		return "~"
	}
	prefix := strings.TrimRight(home, string(os.PathSeparator)) + string(os.PathSeparator)
	if strings.HasPrefix(path, prefix) {
		return "~" + string(os.PathSeparator) + strings.TrimPrefix(path, prefix)
	}
	return path
}
