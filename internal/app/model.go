package app

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"

	"charm.land/bubbles/v2/textarea"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

const (
	minComposerHeight = 3
	maxComposerHeight = 8
	footerRows        = 3
	headerRows        = 4
)

type streamClosedMsg struct{}

type Model struct {
	width    int
	height   int
	ready    bool
	thinking bool

	backend backend.Backend
	session session.AgentSession
	storage storage.Session

	entries []session.Entry
	pending *session.Entry

	viewport viewport.Model
	composer textarea.Model

	lastToolUseID string

	status  string
	workdir string
	branch  string
	sendKey string

	headerStyle    lipgloss.Style
	userStyle      lipgloss.Style
	assistantStyle lipgloss.Style
	systemStyle    lipgloss.Style
	toolStyle      lipgloss.Style
	dimStyle       lipgloss.Style
	lineStyle      lipgloss.Style
}

func New(b backend.Backend, s storage.Session) Model {
	ta := textarea.New()
	ta.Placeholder = "Type a message... (ctrl+s to send)"
	ta.Prompt = "› "
	ta.ShowLineNumbers = false
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
	ta.MaxHeight = maxComposerHeight

	cwd, _ := os.Getwd()
	
	// Load existing entries if available
	var entries []session.Entry
	if s != nil {
		if stored, err := s.Entries(context.Background()); err == nil && len(stored) > 0 {
			entries = stored
		}
	}

	boot := b.Bootstrap()
	if len(entries) == 0 {
		entries = boot.Entries
	}

	vp := viewport.New(viewport.WithWidth(80), viewport.WithHeight(20))
	vp.SoftWrap = true
	vp.MouseWheelEnabled = true
	vp.MouseWheelDelta = 3

	m := Model{
		backend:  b,
		session:  b.Session(),
		storage:  s,
		entries:  entries,
		viewport: vp,
		composer: ta,
		status:   boot.Status,
		workdir:  cwd,
		branch:   currentBranch(),
		sendKey:  "ctrl+s",
		headerStyle: lipgloss.NewStyle().
			Bold(true).
			Foreground(lipgloss.Color("5")).
			PaddingLeft(2),
		userStyle: lipgloss.NewStyle().
			Bold(true).
			PaddingLeft(2),
		assistantStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("6")).
			PaddingLeft(2),
		systemStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("8")).
			Faint(true).
			PaddingLeft(2),
		toolStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("10")).
			PaddingLeft(2),
		dimStyle: lipgloss.NewStyle().
			Faint(true),
		lineStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("8")),
	}
	m.refreshViewport(true)
	return m
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(
		textarea.Blink,
		m.composer.Focus(),
		m.awaitSessionEvent(),
	)
}

func (m Model) awaitSessionEvent() tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-m.session.Events()
		if !ok {
			return streamClosedMsg{}
		}
		return ev
	}
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

	case streamClosedMsg:
		// channel closed, do nothing for now
		return m, nil

	case session.StatusChanged:
		m.status = msg.Status
		return m, m.awaitSessionEvent()

	case session.PlanUpdated:
		// For now, we don't render the plan separately, but we could.
		return m, m.awaitSessionEvent()

	case session.MetadataLoaded:
		return m, m.awaitSessionEvent()

	case session.TurnStarted:
		m.thinking = true
		m.pending = &session.Entry{Role: session.Assistant}
		m.refreshViewport(m.shouldFollowOutput())
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		m.thinking = false
		return m, m.awaitSessionEvent()

	case session.ThinkingDelta:
		follow := m.shouldFollowOutput()
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.Assistant}
		}
		m.pending.Reasoning += msg.Delta
		m.refreshViewport(follow)
		return m, m.awaitSessionEvent()

	case session.AssistantDelta:
		follow := m.shouldFollowOutput()
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.Assistant}
		}
		m.pending.Content += msg.Delta
		m.refreshViewport(follow)
		return m, m.awaitSessionEvent()

	case session.AssistantMessage:
		follow := m.shouldFollowOutput()
		if m.pending != nil {
			if msg.Message != "" {
				m.pending.Content = msg.Message
			}
			m.entries = append(m.entries, *m.pending)

			if m.storage != nil {
				blocks := []storage.Block{}
				if m.pending.Reasoning != "" {
					blocks = append(blocks, storage.Block{
						Type:     "thinking",
						Thinking: &m.pending.Reasoning,
					})
				}
				blocks = append(blocks, storage.Block{
					Type: "text",
					Text: &m.pending.Content,
				})

				m.storage.Append(context.Background(), storage.Assistant{
					Type:    "assistant",
					Content: blocks,
					TS:      time.Now().Unix(),
				})
			}

			m.pending = nil
		}
		m.refreshViewport(follow)
		return m, m.awaitSessionEvent()

	case session.ToolCallStarted:
		// Optionally show a pending tool
		follow := m.shouldFollowOutput()
		m.entries = append(m.entries, session.Entry{
			Role:  session.Tool,
			Title: fmt.Sprintf("%s(%s)", msg.ToolName, msg.Args),
		})

		m.lastToolUseID = session.ShortID()
		if m.storage != nil {
			m.storage.Append(context.Background(), storage.ToolUse{
				Type: "tool_use",
				ID:   m.lastToolUseID,
				Name: msg.ToolName,
				Input: map[string]string{
					"args": msg.Args,
				},
				TS: time.Now().Unix(),
			})
		}

		m.refreshViewport(follow)
		return m, m.awaitSessionEvent()

	case session.ToolOutputDelta:
		follow := m.shouldFollowOutput()
		// Append to the most recent tool entry if it's currently active.
		if len(m.entries) > 0 && m.entries[len(m.entries)-1].Role == session.Tool {
			m.entries[len(m.entries)-1].Content += msg.Delta
		}
		m.refreshViewport(follow)
		return m, m.awaitSessionEvent()

	case session.ToolResult:
		follow := m.shouldFollowOutput()
		// Update the last tool entry or append a new one
		if len(m.entries) > 0 && m.entries[len(m.entries)-1].Role == session.Tool {
			m.entries[len(m.entries)-1].Content = msg.Result
		} else {
			m.entries = append(m.entries, session.Entry{
				Role:    session.Tool,
				Title:   msg.ToolName,
				Content: msg.Result,
			})
		}

		if m.storage != nil {
			m.storage.Append(context.Background(), storage.ToolResult{
				Type:      "tool_result",
				ToolUseID: m.lastToolUseID,
				Content:   msg.Result,
				IsError:   msg.Error != nil,
				TS:        time.Now().Unix(),
			})
		}

		m.refreshViewport(follow)
		return m, m.awaitSessionEvent()

	case session.VerificationResult:
		follow := m.shouldFollowOutput()
		status := "PASSED"
		if !msg.Passed {
			status = "FAILED"
		}
		content := fmt.Sprintf("%s: %s\n%s", status, msg.Metric, msg.Output)
		m.entries = append(m.entries, session.Entry{
			Role:    session.Tool,
			Title:   "verify: " + msg.Command,
			Content: content,
		})
		m.refreshViewport(follow)
		return m, m.awaitSessionEvent()

	case session.Error:
		m.status = fmt.Sprintf("Error: %v", msg.Err)
		return m, m.awaitSessionEvent()

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
				Role:    session.User,
				Content: text,
			})
			if m.storage != nil {
				m.storage.Append(context.Background(), storage.User{
					Type:    "user",
					Content: text,
					TS:      time.Now().Unix(),
				})
			}
			m.composer.Reset()
			m.status = fmt.Sprintf("[%s] turn in flight", m.backend.Name())
			m.layout()
			m.refreshViewport(true)
			m.session.SubmitTurn(context.Background(), text)
			return m, nil
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

	header := m.headerStyle.Render("ion")
	subtitle := m.dimStyle.PaddingLeft(2).Render(fmt.Sprintf("%s  •  %s", m.workdir, m.branch))
	progress := m.progressLine()
	separator := m.lineStyle.Render(strings.Repeat("─", max(0, m.width)))
	status := m.dimStyle.PaddingLeft(2).Render(m.statusLine())

	content := lipgloss.JoinVertical(
		lipgloss.Left,
		header,
		subtitle,
		"",
		m.viewport.View(),
		separator,
		lipgloss.NewStyle().PaddingLeft(2).Render(progress),
		lipgloss.NewStyle().PaddingLeft(1).Render(m.composer.View()),
		separator,
		status,
	)

	return tea.NewView(content)
}

func (m *Model) layout() {
	composerHeight := clamp(m.composer.LineCount()+1, minComposerHeight, maxComposerHeight)
	m.composer.SetWidth(max(20, m.width-4))
	m.composer.SetHeight(composerHeight)

	viewportHeight := max(3, m.height-headerRows-composerHeight-footerRows)
	m.viewport.SetWidth(max(20, m.width-4))
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
	case session.User:
		return m.userStyle.Render("› " + entry.Content)
	case session.Assistant:
		var b strings.Builder
		if entry.Reasoning != "" {
			b.WriteString(m.systemStyle.Render("• Thinking..."))
			b.WriteString("\n")
			b.WriteString(m.dimStyle.PaddingLeft(4).Render(entry.Reasoning))
			b.WriteString("\n\n")
		}
		b.WriteString(m.assistantStyle.Render("• " + entry.Content))
		return b.String()
	case session.Tool:
		label := entry.Title
		if label == "" {
			label = "tool"
		}
		if entry.Content == "" {
			return m.toolStyle.Render("• " + label + " " + m.dimStyle.Render("(pending)"))
		}
		return m.toolStyle.Render("• "+label) + "\n" + m.dimStyle.PaddingLeft(4).Render(entry.Content)
	case session.System:
		return m.systemStyle.Render(entry.Content)
	default:
		return entry.Content
	}
}

func (m Model) progressLine() string {
	if m.thinking {
		if m.pending != nil && m.pending.Content != "" {
			return m.assistantStyle.Render("• Streaming assistant response...")
		}
		return m.assistantStyle.Render(fmt.Sprintf("• Waiting on %s backend...", m.backend.Name()))
	}
	return m.dimStyle.Render("• Ready")
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
