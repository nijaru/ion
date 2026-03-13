package main

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"

	"charm.land/bubbles/v2/textarea"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
)

const (
	minComposerHeight = 3
	maxComposerHeight = 8
	footerRows        = 2
)

type role string

const (
	roleUser      role = "user"
	roleAssistant role = "assistant"
	roleSystem    role = "system"
)

type entry struct {
	role    role
	content string
}

type assistantReplyMsg struct {
	content string
}

type model struct {
	width     int
	height    int
	ready     bool
	thinking  bool
	entries   []entry
	viewport  viewport.Model
	composer  textarea.Model
	status    string
	workdir   string
	branch    string
	sendKey   string
	headerSty lipgloss.Style
	userSty   lipgloss.Style
	asstSty   lipgloss.Style
	sysSty    lipgloss.Style
	dimSty    lipgloss.Style
	lineSty   lipgloss.Style
}

func initialModel() model {
	ta := textarea.New()
	ta.Placeholder = "Type a message... (ctrl+s to send)"
	ta.Prompt = "› "
	ta.ShowLineNumbers = false
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
	ta.Focus()

	cwd, _ := os.Getwd()
	branch := currentBranch()

	m := model{
		entries: []entry{
			{role: roleSystem, content: "ion-go rewrite branch"},
			{role: roleAssistant, content: "This is a Bubble Tea v2 host slice. It simulates turns so we can evaluate transcript, composer, resize, and inline behavior."},
		},
		viewport: viewport.New(viewport.WithWidth(80), viewport.WithHeight(20)),
		composer: ta,
		status:   "[rewrite] Bubble Tea v2 host slice",
		workdir:  cwd,
		branch:   branch,
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

func (m model) Init() tea.Cmd {
	return textarea.Blink
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var cmds []tea.Cmd

	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.ready = true
		m.width = msg.Width
		m.height = msg.Height
		m.layout()
		m.refreshViewport()
		return m, nil

	case assistantReplyMsg:
		m.thinking = false
		m.entries = append(m.entries, entry{role: roleAssistant, content: msg.content})
		m.status = "[reply] fake assistant turn complete"
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
			m.entries = append(m.entries, entry{role: roleUser, content: text})
			m.composer.Reset()
			m.thinking = true
			m.status = "[thinking] fake assistant turn in flight"
			m.layout()
			m.refreshViewport()
			return m, fakeAssistantReply(text)
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

func (m model) View() tea.View {
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

func (m *model) layout() {
	composerHeight := clamp(m.composer.LineCount()+1, minComposerHeight, maxComposerHeight)
	m.composer.SetWidth(max(20, m.width))
	m.composer.SetHeight(composerHeight)

	headerRows := 4
	viewportHeight := max(3, m.height-headerRows-composerHeight-footerRows)
	m.viewport.SetWidth(max(20, m.width))
	m.viewport.SetHeight(viewportHeight)
}

func (m *model) refreshViewport() {
	var b strings.Builder
	for i, entry := range m.entries {
		if i > 0 {
			b.WriteString("\n\n")
		}
		switch entry.role {
		case roleUser:
			b.WriteString(m.userSty.Render("› " + entry.content))
		case roleAssistant:
			b.WriteString(m.asstSty.Render("• " + entry.content))
		case roleSystem:
			b.WriteString(m.sysSty.Render(entry.content))
		}
	}
	m.viewport.SetContent(b.String())
	m.viewport.GotoBottom()
}

func (m model) progressLine() string {
	if m.thinking {
		return m.asstSty.Render("• Thinking... (fake assistant)")
	}
	return m.dimSty.Render("• Ready")
}

func (m model) statusLine() string {
	return fmt.Sprintf(
		"%s • %s • textarea=%d lines • viewport=%d lines",
		m.status,
		m.sendKey+" send",
		m.composer.LineCount(),
		m.viewport.Height,
	)
}

func fakeAssistantReply(input string) tea.Cmd {
	return tea.Tick(350*time.Millisecond, func(time.Time) tea.Msg {
		return assistantReplyMsg{
			content: fmt.Sprintf("Echoing back %q so we can exercise the host loop without a real agent yet.", input),
		}
	})
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

func main() {
	p := tea.NewProgram(initialModel())
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "ion-go error: %v\n", err)
		os.Exit(1)
	}
}
