package app

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

const (
	minComposerHeight = 1
	maxComposerHeight = 10
	footerRows        = 3
	headerRows        = 4
)

type streamClosedMsg struct{}

type toolMode int

const (
	modeRead toolMode = iota
	modeWrite
)

type Model struct {
	width    int
	height   int
	ready    bool
	thinking bool
	mode     toolMode

	backend backend.Backend
	session session.AgentSession
	storage storage.Session

	// entries is now empty by default; history is flushed to scrollback on init.
	entries []session.Entry
	pending *session.Entry

	composer textarea.Model

	lastToolUseID   string
	pendingApproval *session.ApprovalRequest

	status  string
	workdir string
	branch  string
	sendKey string

	headerStyle    lipgloss.Style
	userStyle      lipgloss.Style
	assistantStyle lipgloss.Style
	systemStyle    lipgloss.Style
	toolStyle      lipgloss.Style
	agentStyle     lipgloss.Style
	dimStyle       lipgloss.Style
	lineStyle      lipgloss.Style
	cyanStyle      lipgloss.Style
	warnStyle      lipgloss.Style
}

func New(b backend.Backend, s storage.Session) Model {
	ta := textarea.New()
	ta.Placeholder = "Type a message... (Enter to send)"
	ta.Prompt = "› "
	ta.ShowLineNumbers = false
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
	ta.MaxHeight = maxComposerHeight

	cwd, _ := os.Getwd()

	// Load existing entries for initial flush
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

	m := Model{
		backend:  b,
		session:  b.Session(),
		storage:  s,
		entries:  entries,
		composer: ta,
		status:   boot.Status,
		workdir:  cwd,
		branch:   currentBranch(),
		sendKey:  "enter",
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
		agentStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("13")).
			PaddingLeft(2),
		dimStyle: lipgloss.NewStyle().
			Faint(true),
		lineStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("6")),
		cyanStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("6")),
		warnStyle: lipgloss.NewStyle().
			Foreground(lipgloss.Color("3")),
	}
	return m
}

func (m Model) Init() tea.Cmd {
	// On init, flush the header and any existing history to Plane A.
	header := m.headerStyle.Render("ion")
	subtitle := m.dimStyle.PaddingLeft(2).Render(fmt.Sprintf("%s  •  %s", m.workdir, m.branch))

	var history []string
	history = append(history, header, subtitle, "")
	for _, entry := range m.entries {
		history = append(history, m.renderEntry(entry), "")
	}

	return tea.Batch(
		textarea.Blink,
		m.composer.Focus(),
		tea.Println(strings.Join(history, "\n")),
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
		return m, nil

	case streamClosedMsg:
		return m, nil

	case session.StatusChanged:
		m.status = msg.Status
		return m, m.awaitSessionEvent()

	case session.TurnStarted:
		m.thinking = true
		m.pending = &session.Entry{Role: session.Assistant}
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		m.thinking = false
		return m, m.awaitSessionEvent()

	case session.ThinkingDelta:
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.Assistant}
		}
		m.pending.Reasoning += msg.Delta
		return m, m.awaitSessionEvent()

	case session.AssistantDelta:
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.Assistant}
		}
		m.pending.Content += msg.Delta
		return m, m.awaitSessionEvent()

	case session.AssistantMessage:
		var cmd tea.Cmd
		if m.pending != nil {
			if msg.Message != "" {
				m.pending.Content = msg.Message
			}

			// Flush finalized assistant response to Plane A
			cmd = tea.Println(m.renderEntry(*m.pending) + "\n")

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
		return m, tea.Batch(cmd, m.awaitSessionEvent())

	case session.ToolCallStarted:
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

		// Create a placeholder tool entry to show in Plane B while it runs
		m.pending = &session.Entry{
			Role:  session.Tool,
			Title: fmt.Sprintf("%s(%s)", msg.ToolName, msg.Args),
		}
		return m, m.awaitSessionEvent()

	case session.ToolOutputDelta:
		if m.pending != nil && m.pending.Role == session.Tool {
			m.pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ToolResult:
		var cmd tea.Cmd
		if m.pending != nil && m.pending.Role == session.Tool {
			m.pending.Content = msg.Result
			cmd = tea.Println(m.renderEntry(*m.pending) + "\n")
			m.pending = nil
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
		return m, tea.Batch(cmd, m.awaitSessionEvent())

	case session.VerificationResult:
		status := "PASSED"
		if !msg.Passed {
			status = "FAILED"
		}
		content := fmt.Sprintf("%s: %s\n%s", status, msg.Metric, msg.Output)
		entry := session.Entry{
			Role:    session.Tool,
			Title:   "verify: " + msg.Command,
			Content: content,
		}

		if m.storage != nil {
			m.storage.Append(context.Background(), storage.ToolResult{
				Type:      "tool_result",
				ToolUseID: m.lastToolUseID,
				Content:   content,
				IsError:   !msg.Passed,
				TS:        time.Now().Unix(),
			})
		}
		return m, tea.Batch(tea.Println(m.renderEntry(entry)+"\n"), m.awaitSessionEvent())

	case session.ApprovalRequest:
		m.pendingApproval = &msg
		m.status = "Approval Required (y/n)"
		m.thinking = false
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		m.pending = &session.Entry{
			Role:    session.Agent,
			Title:   msg.AgentName,
			Content: fmt.Sprintf("Query: %s", msg.Query),
		}
		return m, m.awaitSessionEvent()

	case session.ChildDelta:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ChildCompleted:
		var cmd tea.Cmd
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content = msg.Result
			cmd = tea.Println(m.renderEntry(*m.pending) + "\n")
			m.pending = nil
		}
		return m, tea.Batch(cmd, m.awaitSessionEvent())

	case session.ChildFailed:
		var cmd tea.Cmd
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content = "ERROR: " + msg.Error
			cmd = tea.Println(m.renderEntry(*m.pending) + "\n")
			m.pending = nil
		}
		return m, tea.Batch(cmd, m.awaitSessionEvent())

	case session.Error:
		m.status = fmt.Sprintf("Error: %v", msg.Err)
		return m, m.awaitSessionEvent()

	case tea.KeyPressMsg:
		switch msg.String() {
		case "ctrl+c":
			return m, tea.Quit
		case "shift+enter":
			// Shift+Enter inserts a newline in the composer
			var cmd tea.Cmd
			m.composer, cmd = m.composer.Update(msg)
			if m.ready {
				m.layout()
			}
			return m, cmd
		case m.sendKey:
			text := strings.TrimSpace(m.composer.Value())
			if text == "" || m.thinking {
				return m, nil
			}

			userEntry := session.Entry{
				Role:    session.User,
				Content: text,
			}

			// Flush user input to Plane A immediately
			flushCmd := tea.Println(m.renderEntry(userEntry) + "\n")

			if m.storage != nil {
				m.storage.Append(context.Background(), storage.User{
					Type:    "user",
					Content: text,
					TS:      time.Now().Unix(),
				})
			}
			m.composer.Reset()
			m.status = "Turn in flight"
			m.layout()

			if strings.HasPrefix(text, "/") {
				cmd := m.handleCommand(text)
				return m, tea.Batch(flushCmd, cmd)
			}

			m.session.SubmitTurn(context.Background(), text)
			return m, flushCmd
		case "y", "n":
			if m.pendingApproval != nil {
				approved := msg.String() == "y"
				reqID := m.pendingApproval.RequestID
				description := m.pendingApproval.Description

				m.pendingApproval = nil
				m.status = "Processing approval..."

				systemEntry := session.Entry{
					Role:    session.System,
					Content: fmt.Sprintf("Host %s: %s", ifthen(approved, "approved", "denied"), description),
				}

				m.session.Approve(context.Background(), reqID, approved)
				return m, tea.Println(m.renderEntry(systemEntry) + "\n")
			}
		case "shift+tab":
			if m.mode == modeWrite {
				m.mode = modeRead
			} else {
				m.mode = modeWrite
			}
			return m, nil
		}
	}

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

	// Plane B: Ephemeral active area
	var b strings.Builder
	if m.pending != nil {
		b.WriteString(m.renderEntry(*m.pending))
		b.WriteString("\n\n")
	}

	progress := m.progressLine()
	separator := m.lineStyle.Render(strings.Repeat("─", max(0, m.width)))
	status := m.statusLine()

	// Bottom UI layout:
	// [ progress  ]
	// [ separator ]
	// [ composer  ]
	// [ separator ]
	// [ status    ]
	content := lipgloss.JoinVertical(
		lipgloss.Left,
		b.String(),
		progress,
		separator,
		lipgloss.NewStyle().PaddingLeft(1).Render(m.composer.View()),
		separator,
		status,
	)

	return tea.NewView(content)
}
func (m *Model) handleCommand(input string) tea.Cmd {
	fields := strings.Fields(input)
	if len(fields) == 0 {
		return nil
	}

	cmd := fields[0]
	switch cmd {
	case "/mcp":
		if len(fields) < 3 || fields[1] != "add" {
			return func() tea.Msg {
				return session.Error{Err: fmt.Errorf("usage: /mcp add <command> [args...]")}
			}
		}
		mcpCmd := fields[2]
		mcpArgs := fields[3:]
		return func() tea.Msg {
			if err := m.session.RegisterMCPServer(context.Background(), mcpCmd, mcpArgs...); err != nil {
				return session.Error{Err: err}
			}
			return nil
		}
	case "/exit", "/quit":
		return tea.Quit
	default:
		return func() tea.Msg {
			return session.Error{Err: fmt.Errorf("unknown command: %s", cmd)}
		}
	}
}

func (m *Model) layout() {
	m.composer.SetWidth(max(20, m.width-4))
	m.composer.SetHeight(clamp(m.composer.LineCount(), minComposerHeight, maxComposerHeight))
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
	case session.Agent:
		label := entry.Title
		if label == "" {
			label = "agent"
		}
		return m.agentStyle.Render("🤖 "+label) + "\n" + m.dimStyle.PaddingLeft(4).Render(entry.Content)
	case session.System:
		return m.systemStyle.Render(entry.Content)
	default:
		return entry.Content
	}
}

func (m Model) progressLine() string {
	if m.thinking {
		// Check for active child agent
		if m.pending != nil && m.pending.Role == session.Agent {
			return m.agentStyle.Render(fmt.Sprintf("  🤖 Agent %s is working...", m.pending.Title))
		}

		if m.pending != nil && m.pending.Content != "" {
			return m.cyanStyle.Render("  · Streaming assistant response...")
		}
		return m.cyanStyle.Render(fmt.Sprintf("  · Waiting on %s backend...", m.backend.Name()))
	}
	return m.dimStyle.Render("  · Ready")
}

func (m Model) statusLine() string {
	modelName := os.Getenv("ION_MODEL")
	if modelName == "" {
		modelName = "openrouter minimax/minimax-m2.7"
	}

	sep := m.dimStyle.Render(" · ")

	var segments []string

	// Mode indicator: [WRITE] yellow, [READ] cyan
	if m.mode == modeWrite {
		segments = append(segments, m.warnStyle.Render("[WRITE]"))
	} else {
		segments = append(segments, m.cyanStyle.Render("[READ]"))
	}

	segments = append(segments, modelName)

	dirName := "./" + filepath.Base(m.workdir)
	segments = append(segments, dirName)

	if m.branch != "" {
		segments = append(segments, m.cyanStyle.Render(m.branch))
	}

	if m.composer.Value() != "" {
		segments = append(segments, m.dimStyle.Render(fmt.Sprintf("draft:%d", m.composer.LineCount())))
	}

	return "  " + strings.Join(segments, sep)
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

func ifthen[T any](cond bool, a, b T) T {
	if cond {
		return a
	}
	return b
}
