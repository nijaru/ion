package app

import (
	"fmt"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/internal/session"
)

func (m Model) View() tea.View {
	if !m.ready {
		return tea.NewView("loading...")
	}

	var b strings.Builder

	// Blank line separates scrollback from dynamic area
	b.WriteString("\n")

	// Plane B — ephemeral in-flight content
	planeB := m.renderPlaneB()
	if planeB != "" {
		b.WriteString(planeB)
	}

	// Progress line
	b.WriteString(m.progressLine())
	b.WriteString("\n")

	// Top separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.width))))
	b.WriteString("\n")

	// Composer
	b.WriteString(lipgloss.NewStyle().PaddingLeft(1).Render(m.composer.View()))
	b.WriteString("\n")

	// Bottom separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.width))))
	b.WriteString("\n")

	// Status line
	b.WriteString(m.statusLine())

	return tea.NewView(b.String())
}

// renderPlaneB renders all ephemeral in-flight content.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	if m.pending == nil && m.pendingApproval == nil && m.reasonBuf == "" {
		return ""
	}

	var b strings.Builder

	// Thinking/reasoning (dimmed, shown while generating)
	if m.reasonBuf != "" {
		b.WriteString(m.st.dim.Render("  • Thinking..."))
		b.WriteString("\n")
		for _, line := range strings.Split(m.reasonBuf, "\n") {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(line))
			b.WriteString("\n")
		}
	}

	// Active in-flight entry (streaming assistant, tool, or agent)
	if m.pending != nil {
		b.WriteString(m.renderPendingEntry(*m.pending))
		b.WriteString("\n")
	}

	// Approval prompt
	if m.pendingApproval != nil {
		b.WriteString("\n")
		desc := m.pendingApproval.Description
		if m.pendingApproval.ToolName != "" {
			desc = fmt.Sprintf("%s(%s): %s",
				m.pendingApproval.ToolName,
				m.pendingApproval.Args,
				m.pendingApproval.Description)
		}
		b.WriteString(m.st.warn.PaddingLeft(2).Render("Approve " + desc + "? (y/n)"))
		b.WriteString("\n")
	}

	return b.String()
}

// renderPendingEntry renders an in-flight entry for Plane B.
func (m Model) renderPendingEntry(e session.Entry) string {
	switch e.Role {
	case session.Assistant:
		if e.Content == "" {
			return m.st.dim.PaddingLeft(2).Render("• ...")
		}
		return m.st.assistant.Render("• " + e.Content)
	case session.Tool:
		label := e.Title
		if label == "" {
			label = "tool"
		}
		var b strings.Builder
		b.WriteString(m.st.tool.Render("• " + label))
		if e.Content != "" {
			b.WriteString("\n")
			lines := strings.Split(strings.TrimRight(e.Content, "\n"), "\n")
			shown := lines
			if len(lines) > 10 {
				shown = lines[:10]
			}
			for _, l := range shown {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(l))
				b.WriteString("\n")
			}
			if len(lines) > 10 {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(
					fmt.Sprintf("... (%d more lines)", len(lines)-10)))
				b.WriteString("\n")
			}
		}
		return b.String()
	case session.Agent:
		label := e.Title
		if label == "" {
			label = "agent"
		}
		var b strings.Builder
		b.WriteString(m.st.agent.Render("↳ " + label))
		if e.Content != "" {
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Content))
		}
		return b.String()
	default:
		return e.Content
	}
}

// renderEntry formats a completed entry for tea.Printf scrollback commit.
func (m Model) renderEntry(e session.Entry) string {
	switch e.Role {
	case session.User:
		return m.st.user.Render("› " + e.Content)

	case session.Assistant:
		var b strings.Builder
		if e.Reasoning != "" {
			b.WriteString(m.st.system.Render("• Thinking"))
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Reasoning))
			b.WriteString("\n")
		}
		b.WriteString(m.st.assistant.Render("• " + e.Content))
		return b.String()

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
		if e.Content == "" {
			return labelStr
		}
		content := e.Content
		if isWriteTool(e.Title) {
			content = m.renderDiff(content)
		}
		var b strings.Builder
		b.WriteString(labelStr)
		b.WriteString("\n")
		lines := strings.Split(strings.TrimRight(content, "\n"), "\n")
		shown := lines
		if len(lines) > 10 {
			shown = lines[:10]
		}
		for _, l := range shown {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(l))
			b.WriteString("\n")
		}
		if len(lines) > 10 {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(
				fmt.Sprintf("... (%d more lines)", len(lines)-10)))
		}
		return b.String()

	case session.Agent:
		label := e.Title
		if label == "" {
			label = "agent"
		}
		var b strings.Builder
		b.WriteString(m.st.agent.Render("↳ " + label))
		b.WriteString("\n")
		b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Content))
		return b.String()

	case session.System:
		return m.st.system.Render("  " + e.Content)

	default:
		return e.Content
	}
}

// progressLine renders the single-line progress indicator between Plane B and the composer.
func (m Model) progressLine() string {
	switch m.progress {
	case stateIonizing:
		return m.st.cyan.Render("  " + m.spinner.View() + " Ionizing...")
	case stateStreaming:
		return m.st.cyan.Render("  " + m.spinner.View() + " Streaming...")
	case stateWorking:
		return m.st.cyan.Render("  " + m.spinner.View() + " Working...")
	case stateApproval:
		return m.st.warn.Render("  ⚠ Approval required")
	case stateCancelled:
		return m.st.dim.Render("  · Cancelled")
	case stateError:
		return m.st.warn.Render("  ✗ Error: " + m.lastError)
	default:
		return m.st.dim.Render("  · Ready")
	}
}

// statusLine renders the bottom info bar.
func (m Model) statusLine() string {
	sep := m.st.dim.Render(" · ")

	modeLabel := ifthen(m.mode == modeWrite,
		m.st.modeWrite.Render("[WRITE]"),
		m.st.modeRead.Render("[READ]"),
	)

	provider := m.backend.Provider()
	model := m.backend.Model()

	var segments []string
	segments = append(segments, modeLabel)
	if provider != "" {
		segments = append(segments, m.st.dim.Render(provider))
	}
	if model != "" {
		segments = append(segments, m.st.dim.Render(model))
	}

	// Token usage
	total := m.tokensSent + m.tokensReceived
	limit := m.backend.ContextLimit()
	var usage string
	if limit > 0 {
		pct := (total * 100) / limit
		usage = fmt.Sprintf("%dk/%dk (%d%%)", total/1000, limit/1000, pct)
	} else if total > 0 {
		usage = fmt.Sprintf("%dk tokens", total/1000)
	} else {
		usage = "0 tokens"
	}
	segments = append(segments, usage)

	if m.totalCost > 0 {
		segments = append(segments, fmt.Sprintf("$%.3f", m.totalCost))
	}

	if m.width > 80 {
		segments = append(segments, "./"+filepath.Base(m.workdir))
	}
	if m.width > 60 && m.branch != "" {
		segments = append(segments, m.st.cyan.Render(m.branch))
	}

	return "  " + strings.Join(segments, sep)
}

// layout recomputes widget dimensions based on current terminal size.
func (m *Model) layout() {
	m.composer.SetWidth(max(20, m.width-4))
	m.composer.SetHeight(clamp(m.composer.LineCount(), minComposerHeight, maxComposerHeight))
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
