package app

import (
	"fmt"
	"os"
	"os/exec"
	"strings"

	"charm.land/bubbles/v2/textarea"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/go-host/internal/backend"
	"github.com/nijaru/ion/go-host/internal/session"
)

const (
	minComposerHeight = 3
	maxComposerHeight = 8
	footerRows        = 3
	headerRows        = 4
)

type Model struct {
	width    int
	height   int
	ready    bool
	thinking bool

	backend backend.Backend
	entries []session.Entry
	pending *session.Entry

	viewport viewport.Model
	composer textarea.Model

	status  string
	workdir string
	branch  string
	sendKey string

	headerSty lipgloss.Style
	userSty   lipgloss.Style
	asstSty   lipgloss.Style
	sysSty    lipgloss.Style
	toolSty   lipgloss.Style
	dimSty    lipgloss.Style
	lineSty   lipgloss.Style
}

func New(backend backend.Backend) Model {
	ta := textarea.New()
	ta.Placeholder = "Type a message... (ctrl+s to send)"
	ta.Prompt = "› "
	ta.ShowLineNumbers = false
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
	ta.MaxHeight = maxComposerHeight

	cwd, _ := os.Getwd()
	boot := backend.Bootstrap()
	vp := viewport.New(viewport.WithWidth(80), viewport.WithHeight(20))
	vp.SoftWrap = true
	vp.MouseWheelEnabled = true
	vp.MouseWheelDelta = 3

	m := Model{
		backend:  backend,
		entries:  boot.Entries,
		viewport: vp,
		composer: ta,
		status:   boot.Status,
		workdir:  cwd,
		branch:   currentBranch(),
		sendKey:  "ctrl+s",
		headerSty: lipgloss.NewStyle().
			Bold(true),
		userSty: lipgloss.NewStyle().
			Bold(true),
		asstSty: lipgloss.NewStyle().
			Foreground(lipgloss.Color("6")),
		sysSty: lipgloss.NewStyle().
			Foreground(lipgloss.Color("8")),
		toolSty: lipgloss.NewStyle().
			Foreground(lipgloss.Color("10")),
		dimSty: lipgloss.NewStyle().
			Faint(true),
		lineSty: lipgloss.NewStyle().
			Foreground(lipgloss.Color("8")),
	}
	m.refreshViewport(true)
	return m
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(textarea.Blink, m.composer.Focus())
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var cmds []tea.Cmd

	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.ready = true
		m.width = msg.Width
		m.height = msg.Height
		m.layout()
		m.refreshViewport(true)
		return m, nil

	case backend.StatusMsg:
		m.status = msg.Text
		return m, nil

	case backend.TurnStateMsg:
		m.thinking = msg.Running
		return m, nil

	case backend.StreamStartMsg:
		follow := m.shouldFollowOutput()
		m.pending = &session.Entry{Role: msg.Role}
		m.refreshViewport(follow)
		return m, nil

	case backend.StreamDeltaMsg:
		follow := m.shouldFollowOutput()
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.RoleAssistant}
		}
		m.pending.Content += msg.Delta
		m.refreshViewport(follow)
		return m, nil

	case backend.StreamDoneMsg:
		follow := m.shouldFollowOutput()
		if m.pending != nil {
			m.entries = append(m.entries, *m.pending)
			m.pending = nil
		}
		m.refreshViewport(follow)
		return m, nil

	case backend.AppendEntryMsg:
		follow := m.shouldFollowOutput()
		m.entries = append(m.entries, msg.Entry)
		m.refreshViewport(follow)
		return m, nil

	case tea.KeyPressMsg:
		switch msg.String() {
		case "ctrl+c":
			return m, tea.Quit
		case m.sendKey:
			text := strings.TrimSpace(m.composer.Value())
			if text == "" || m.thinking {
				return m, nil
			}
			m.entries = append(m.entries, session.Entry{
				Role:    session.RoleUser,
				Content: text,
			})
			m.composer.Reset()
			m.thinking = true
			m.status = fmt.Sprintf("[%s] turn in flight", m.backend.Name())
			m.pending = nil
			m.layout()
			m.refreshViewport(true)
			return m, m.backend.Submit(text)
		case "pgup":
			m.viewport.PageUp()
			return m, nil
		case "pgdn":
			m.viewport.PageDown()
			return m, nil
		case "home":
			m.viewport.GotoTop()
			return m, nil
		case "end":
			m.viewport.GotoBottom()
			return m, nil
		}
	}

	m.viewport, _ = m.viewport.Update(msg)
	var cmd tea.Cmd
	m.composer, cmd = m.composer.Update(msg)
	cmds = append(cmds, cmd)

	if m.ready {
		m.layout()
	}

	return m, tea.Batch(cmds...)
}

func (m Model) View() tea.View {
	if !m.ready {
		return tea.NewView("loading...")
	}

	header := m.headerSty.Render("ion-go")
	subtitle := m.dimSty.Render(fmt.Sprintf("%s  •  %s", m.workdir, m.branch))
	progress := m.progressLine()
	separator := m.lineSty.Render(strings.Repeat("─", max(0, m.width-1)))
	status := m.dimSty.Render(m.statusLine())

	content := lipgloss.JoinVertical(
		lipgloss.Left,
		header,
		subtitle,
		"",
		m.viewport.View(),
		separator,
		progress,
		m.composer.View(),
		separator,
		status,
	)

	return tea.NewView(content)
}

func (m *Model) layout() {
	composerHeight := clamp(m.composer.LineCount()+1, minComposerHeight, maxComposerHeight)
	m.composer.SetWidth(max(20, m.width))
	m.composer.SetHeight(composerHeight)

	viewportHeight := max(3, m.height-headerRows-composerHeight-footerRows)
	m.viewport.SetWidth(max(20, m.width))
	m.viewport.SetHeight(viewportHeight)
}

func (m *Model) refreshViewport(follow bool) {
	var b strings.Builder
	for i, entry := range m.renderEntries() {
		if i > 0 {
			b.WriteString("\n\n")
		}
		b.WriteString(m.renderEntry(entry))
	}
	m.viewport.SetContent(b.String())
	if follow {
		m.viewport.GotoBottom()
	}
}

func (m Model) renderEntries() []session.Entry {
	entries := make([]session.Entry, 0, len(m.entries)+1)
	entries = append(entries, m.entries...)
	if m.pending != nil {
		entries = append(entries, *m.pending)
	}
	return entries
}

func (m Model) renderEntry(entry session.Entry) string {
	switch entry.Role {
	case session.RoleUser:
		return m.userSty.Render("› " + entry.Content)
	case session.RoleAssistant:
		return m.asstSty.Render("• " + entry.Content)
	case session.RoleTool:
		label := entry.Title
		if label == "" {
			label = "tool"
		}
		if entry.Content == "" {
			return m.toolSty.Render("• " + label)
		}
		return m.toolSty.Render("• "+label) + "\n" + m.dimSty.Render(indentBlock(entry.Content, "  "))
	case session.RoleSystem:
		return m.sysSty.Render(entry.Content)
	default:
		return entry.Content
	}
}

func indentBlock(content, prefix string) string {
	lines := strings.Split(content, "\n")
	for i := range lines {
		lines[i] = prefix + lines[i]
	}
	return strings.Join(lines, "\n")
}

func (m Model) progressLine() string {
	if m.thinking {
		if m.pending != nil && m.pending.Content != "" {
			return m.asstSty.Render("• Streaming assistant response...")
		}
		return m.asstSty.Render(fmt.Sprintf("• Waiting on %s backend...", m.backend.Name()))
	}
	return m.dimSty.Render("• Ready")
}

func (m Model) statusLine() string {
	return fmt.Sprintf(
		"%s • backend=%s • %s • draft=%d lines • transcript=%d%%",
		m.status,
		m.backend.Name(),
		m.sendKey+" send",
		m.composer.LineCount(),
		int(m.viewport.ScrollPercent()*100),
	)
}

func (m Model) shouldFollowOutput() bool {
	if !m.ready {
		return true
	}
	return m.viewport.AtBottom() || m.viewport.PastBottom()
}

func currentBranch() string {
	out, err := exec.Command("git", "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
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

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}
