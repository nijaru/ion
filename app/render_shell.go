package app

import (
	"fmt"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/nijaru/ion/internal/terminal"
)

type styles struct {
	user     lipgloss.Style
	agent    lipgloss.Style
	system   lipgloss.Style
	tool     lipgloss.Style
	subagent lipgloss.Style
	success  lipgloss.Style
	dim      lipgloss.Style
	cyan     lipgloss.Style
	warn     lipgloss.Style
	caution  lipgloss.Style
	sep      lipgloss.Style
	added    lipgloss.Style
	removed  lipgloss.Style
	modeRead lipgloss.Style
	modeEdit lipgloss.Style
	modeYolo lipgloss.Style
}

func newStyles() styles {
	return styles{
		user:     lipgloss.NewStyle().Faint(true),
		agent:    lipgloss.NewStyle(),
		system:   lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Faint(true),
		tool:     lipgloss.NewStyle().Foreground(lipgloss.Color("10")),
		subagent: lipgloss.NewStyle().Foreground(lipgloss.Color("13")),
		success:  lipgloss.NewStyle().Foreground(lipgloss.Color("2")),
		dim:      lipgloss.NewStyle().Faint(true),
		cyan:     lipgloss.NewStyle().Foreground(lipgloss.Color("6")),
		warn:     lipgloss.NewStyle().Foreground(lipgloss.Color("1")),
		caution:  lipgloss.NewStyle().Foreground(lipgloss.Color("3")),
		sep:      lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Faint(true),
		added:    lipgloss.NewStyle().Foreground(lipgloss.Color("2")),
		removed:  lipgloss.NewStyle().Foreground(lipgloss.Color("1")),
		modeRead: lipgloss.NewStyle().Foreground(lipgloss.Color("6")).Bold(true),
		modeEdit: lipgloss.NewStyle().
			Foreground(lipgloss.Color("2")).
			Bold(true),
		modeYolo: lipgloss.NewStyle().
			Foreground(lipgloss.Color("1")).
			Bold(true),
	}
}

func (m Model) View() tea.View {
	if m.Picker.PreStartupMode {
		var v tea.View
		if !m.App.Ready || m.Picker.Session == nil {
			v = tea.NewView("loading...")
		} else {
			v = tea.NewView(m.renderSessionPicker())
		}
		v.AltScreen = true
		return v
	}

	if !m.App.Ready {
		return tea.NewView("loading...")
	}

	var b strings.Builder

	// Plane B — ephemeral in-flight content
	planeB := m.renderPlaneB()
	hasShellLeadIn := false
	if planeB != "" {
		b.WriteString(planeB)
		hasShellLeadIn = true
	}

	// Selection overlay
	if m.Picker.Approval != nil {
		b.WriteString(m.renderApprovalPrompt())
		b.WriteString("\n")
		hasShellLeadIn = true
	} else if m.Picker.Session != nil {
		b.WriteString(m.renderSessionPicker())
		b.WriteString("\n")
		hasShellLeadIn = true
	} else if m.Picker.UserMessage != nil {
		b.WriteString(m.renderUserMessageForkPicker())
		b.WriteString("\n")
		hasShellLeadIn = true
	} else if m.Picker.Tree != nil {
		if m.Picker.BranchSummary != nil {
			b.WriteString(m.renderBranchSummaryPrompt())
		} else {
			b.WriteString(m.renderTreePicker())
		}
		b.WriteString("\n")
		hasShellLeadIn = true
	} else if m.Picker.Setup != nil {
		b.WriteString(m.renderSetupPrompt())
		b.WriteString("\n")
		hasShellLeadIn = true
	} else if m.Picker.Overlay != nil {
		b.WriteString(m.renderPicker())
		b.WriteString("\n")
		hasShellLeadIn = true
	}

	if hasShellLeadIn && !strings.HasSuffix(b.String(), "\n\n") {
		b.WriteString("\n")
	}

	if queued := m.renderQueuedTurns(); queued != "" {
		b.WriteString(queued)
		b.WriteString("\n\n")
	}

	b.WriteString(m.renderShell())
	oscHeader := terminal.SetWindowTitle(m.terminalTitle()) +
		terminal.ProgressSequence(m.terminalIsBusy(), m.Progress.Mode == StateError)
	return tea.NewView(oscHeader + b.String())
}

func (m Model) terminalTitle() string {
	name := m.App.SessionName
	if name == "" {
		if m.App.Workdir != "" {
			name = filepath.Base(m.App.Workdir)
		} else {
			name = "ion"
		}
	}
	if m.terminalIsBusy() {
		return fmt.Sprintf("ion [busy] • %s", name)
	}
	return fmt.Sprintf("ion • %s", name)
}

func (m Model) terminalIsBusy() bool {
	return m.InFlight.ReasonBuf != "" ||
		m.InFlight.Pending != nil ||
		len(m.InFlight.PendingTools) > 0 ||
		m.Progress.Mode == StateIonizing ||
		m.Progress.Mode == StateStreaming ||
		m.Progress.Mode == StateWorking ||
		m.Progress.Compacting
}

func (m Model) renderShell() string {
	var b strings.Builder

	// Progress line — suppress when Plane B already shows thinking content
	if m.InFlight.ReasonBuf == "" {
		if progress := m.progressLine(); progress != "" {
			b.WriteString(progress)
			b.WriteString("\n")
		}
	}

	b.WriteString(m.st.sep.Render(m.shellSeparator()))
	b.WriteString("\n")

	// Composer
	b.WriteString(m.renderComposer())
	b.WriteString("\n")
	if completions := m.renderComposerCompletions(); completions != "" {
		b.WriteString(completions)
		b.WriteString("\n")
	}

	// Bottom separator
	b.WriteString(m.st.sep.Render(m.shellSeparator()))
	b.WriteString("\n")

	// Status line
	b.WriteString(m.statusLine())

	return b.String()
}

func (m Model) shellWidth() int {
	if m.App.Width <= 1 {
		return max(0, m.App.Width)
	}
	// Inline terminal rows that exactly fill the terminal can auto-wrap into an
	// extra physical row. Keep live shell chrome one cell short so resize redraws
	// do not leave stale progress/status fragments behind.
	return m.App.Width - 1
}

func (m Model) shellSeparator() string {
	width := m.shellWidth()
	if width <= 0 {
		return ""
	}
	return strings.Repeat("─", width)
}

func (m Model) shellPaddedLine(style lipgloss.Style, text string) string {
	width := m.shellWidth()
	if width <= 0 {
		return ""
	}
	if width <= 2 {
		return fitLine(style.Render(text), width)
	}
	return style.PaddingLeft(2).Render(fitLine(text, width-2))
}
