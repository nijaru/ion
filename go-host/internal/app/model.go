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
	footerRows        = 2
)

type Model struct {
	width    int
	height   int
	ready    bool
	thinking bool

	backend  backend.Backend
	entries  []session.Entry
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
	ta.Focus()

	cwd, _ := os.Getwd()
	entries, status := backend.Bootstrap()

	m := Model{
		backend:  backend,
		entries:  entries,
		viewport: viewport.New(viewport.WithWidth(80), viewport.WithHeight(20)),
		composer: ta,
		status:   status,
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
		dimSty: lipgloss.NewStyle().
			Faint(true),
		lineSty: lipgloss.NewStyle().
			Foreground(lipgloss.Color("8")),
	}
	m.refreshViewport()
	return m
}

func (m Model) Init() tea.Cmd {
	return textarea.Blink
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var cmds []tea.Cmd

	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.ready = true
		m.width = msg.Width
		m.height = msg.Height
		m.layout()
		m.refreshViewport()
		return m, nil

	case backend.ReplyMsg:
		m.thinking = false
		m.entries = append(m.entries, msg.Entry)
		m.status = msg.Status
		m.refreshViewport()
		return m, nil

	case tea.KeyPressMsg:
		switch {
		case msg.String() == "ctrl+c":
			return m, tea.Quit
		case msg.String() == m.sendKey:
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
			m.layout()
			m.refreshViewport()
			return m, m.backend.Submit(text)
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

	headerRows := 4
	viewportHeight := max(3, m.height-headerRows-composerHeight-footerRows)
	m.viewport.SetWidth(max(20, m.width))
	m.viewport.SetHeight(viewportHeight)
}

func (m *Model) refreshViewport() {
	var b strings.Builder
	for i, entry := range m.entries {
		if i > 0 {
			b.WriteString("\n\n")
		}
		switch entry.Role {
		case session.RoleUser:
			b.WriteString(m.userSty.Render("› " + entry.Content))
		case session.RoleAssistant:
			b.WriteString(m.asstSty.Render("• " + entry.Content))
		case session.RoleSystem:
			b.WriteString(m.sysSty.Render(entry.Content))
		}
	}
	m.viewport.SetContent(b.String())
	m.viewport.GotoBottom()
}

func (m Model) progressLine() string {
	if m.thinking {
		return m.asstSty.Render(fmt.Sprintf("• Waiting on %s backend...", m.backend.Name()))
	}
	return m.dimSty.Render("• Ready")
}

func (m Model) statusLine() string {
	return fmt.Sprintf(
		"%s • backend=%s • %s • textarea=%d lines • viewport=%d lines",
		m.status,
		m.backend.Name(),
		m.sendKey+" send",
		m.composer.LineCount(),
		m.viewport.Height(),
	)
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
