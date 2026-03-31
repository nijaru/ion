package app

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/session"
)

// Input handles the user input composer, history, and the bottom status bar.
type Input struct{}

func (m Model) statusLine() string {
	if hint := strings.TrimSpace(m.pendingActionStatus()); hint != "" {
		return fitLine(m.st.warn.Render(hint), m.width)
	}

	sep := m.st.sep.Render(" • ")

	var modeLabel string
	switch m.mode {
	case session.ModeRead:
		modeLabel = m.st.modeRead.Render("[READ]")
	case session.ModeEdit:
		modeLabel = m.st.modeEdit.Render("[EDIT]")
	case session.ModeYolo:
		modeLabel = m.st.modeYolo.Render("[YOLO]")
	}

	provider := ""
	if value := m.backend.Provider(); value != "" {
		provider = m.st.dim.Render(value)
	}
	model := ""
	if value := m.backend.Model(); value != "" {
		model = m.st.dim.Render(value)
	}
	thinking := m.st.dim.Render(normalizeThinkingValue(m.reasoningEffort))
	dir := m.st.dim.Render("./" + filepath.Base(m.workdir))
	branch := ""
	if m.branch != "" {
		branch = m.st.dim.Render(m.branch)
	}

	total := m.tokensSent + m.tokensReceived
	limit := m.backend.ContextLimit()
	var usage string
	if total > 0 && limit > 0 {
		pct := (total * 100) / limit
		usage = m.st.dim.Render(fmt.Sprintf("%dk/%dk (%d%%)", total/1000, limit/1000, pct))
	} else if total > 0 {
		usage = m.st.dim.Render(fmt.Sprintf("%dk tokens", total/1000))
	}

	cost := ""
	if m.totalCost > 0 {
		cost = m.st.dim.Render(fmt.Sprintf("$%.3f", m.totalCost))
	}

	candidates := [][]string{
		{modeLabel, provider, model, thinking, usage, cost, dir, branch},
		{modeLabel, provider, model, thinking, usage, cost, branch},
		{modeLabel, provider, model, thinking, usage, cost},
		{modeLabel, model, thinking, usage, cost},
		{modeLabel, thinking, usage, cost},
	}
	for _, segments := range candidates {
		line := joinLineSegments(sep, segments...)
		if ansi.StringWidth(line) <= m.width {
			return line
		}
	}

	return fitLine(joinLineSegments(sep, modeLabel, thinking, usage, cost), m.width)
}

func (m *Model) layout() {
	m.composer.SetWidth(max(20, m.width-4))
	m.composer.SetHeight(clamp(m.composer.LineCount(), minComposerHeight, maxComposerHeight))
}

func (m Model) headerLine() string {
	return m.headerLineFor(m.branch)
}

func (m Model) headerLineFor(branch string) string {
	sep := m.st.dim.Render(" • ")

	home, _ := os.UserHomeDir()
	dir := m.workdir
	if home != "" && strings.HasPrefix(dir, home) {
		dir = "~" + dir[len(home):]
	}

	pathParts := []string{m.st.dim.Render(dir)}
	if branch != "" {
		pathParts = append(pathParts, m.st.dim.Render(branch))
	}
	return strings.Join(pathParts, sep)
}
