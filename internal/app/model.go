package app

import (
	"context"
	"time"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

const (
	minComposerHeight = 1
	maxComposerHeight = 10
)

type streamClosedMsg struct{}

type runtimeSwitcher func(context.Context, *config.Config) (backend.Backend, session.AgentSession, storage.Session, error)

type runtimeSwitchedMsg struct {
	backend backend.Backend
	session session.AgentSession
	storage storage.Session
	status  string
	notice  string
}

type pickerPurpose int

const (
	pickerPurposeProvider pickerPurpose = iota
	pickerPurposeModel
)

type pickerItem struct {
	Label  string
	Value  string
	Detail string
}

type pickerState struct {
	title   string
	items   []pickerItem
	index   int
	purpose pickerPurpose
	cfg     *config.Config
}

type toolMode int

const (
	modeRead toolMode = iota
	modeWrite
)

type progressState int

const (
	stateReady progressState = iota
	stateIonizing
	stateStreaming
	stateWorking
	stateApproval
	stateCancelled
	stateError
)

// Model is the Bubble Tea model for the ion TUI.
// Rendering is in render.go; event handling is in events.go.
type Model struct {
	width  int
	height int
	ready  bool

	// Backend and session
	backend  backend.Backend
	session  session.AgentSession
	storage  storage.Session
	switcher runtimeSwitcher

	// In-flight state — Plane B content
	pending   *session.Entry // streaming assistant, active tool, or active agent
	reasonBuf string         // accumulates ThinkingDelta
	streamBuf string         // accumulates AssistantDelta (mirrors pending.Content)

	// Approval
	pendingApproval *session.ApprovalRequest

	// Selection overlay
	picker *pickerState

	// Progress and status
	progress  progressState
	lastError string
	thinking  bool

	// Token / cost tracking
	tokensSent     int
	tokensReceived int
	totalCost      float64

	// Composer
	composer textarea.Model
	spinner  spinner.Model

	// Input history
	history      []string
	historyIdx   int
	historyDraft string

	// Double-tap tracking
	escPending   bool
	ctrlCPending bool

	// Storage correlation
	lastToolUseID string

	// Workspace metadata
	status  string
	workdir string
	branch  string
	version string
	mode    toolMode

	// Styles (initialized once in New)
	st styles
}

func New(b backend.Backend, s storage.Session, workdir, branch, version string, switcher runtimeSwitcher) Model {
	ta := textarea.New()
	ta.Placeholder = "Type a message..."
	ta.Prompt = "› "
	ta.ShowLineNumbers = false
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
	ta.MaxHeight = maxComposerHeight

	st := newStyles()

	spt := spinner.New()
	spt.Spinner = spinner.Dot
	spt.Style = st.cyan

	boot := b.Bootstrap()

	m := Model{
		backend:    b,
		session:    b.Session(),
		storage:    s,
		switcher:   switcher,
		composer:   ta,
		spinner:    spt,
		status:     boot.Status,
		workdir:    workdir,
		branch:     branch,
		version:    version,
		historyIdx: -1,
		st:         st,
	}

	if s != nil {
		if input, output, cost, err := s.Usage(context.Background()); err == nil {
			m.tokensSent = input
			m.tokensReceived = output
			m.totalCost = cost
		}
	}

	return m
}

func (m Model) Init() tea.Cmd {
	cmds := []tea.Cmd{
		tea.Printf("%s\n", m.headerLine()),
		textarea.Blink,
		m.spinner.Tick,
		m.composer.Focus(),
		m.awaitSessionEvent(),
	}
	if m.status != "" {
		cmds = append(cmds, tea.Printf("%s\n", m.st.dim.Render("  "+m.status)))
	}
	return tea.Batch(cmds...)
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
	switch msg := msg.(type) {
	case spinner.TickMsg:
		var cmd tea.Cmd
		m.spinner, cmd = m.spinner.Update(msg)
		return m, cmd

	case tea.WindowSizeMsg:
		m.ready = true
		m.width = msg.Width
		m.height = msg.Height
		m.layout()
		return m, nil

	case streamClosedMsg:
		return m, nil

	case runtimeSwitchedMsg:
		m.backend = msg.backend
		m.session = msg.session
		m.storage = msg.storage
		m.picker = nil
		m.status = msg.status
		if msg.storage != nil {
			meta := msg.storage.Meta()
			m.branch = meta.Branch
		}
		m.pending = nil
		m.pendingApproval = nil
		m.reasonBuf = ""
		m.streamBuf = ""
		m.progress = stateReady
		m.lastError = ""
		m.thinking = false
		m.ctrlCPending = false
		m.escPending = false
		m.tokensSent = 0
		m.tokensReceived = 0
		m.totalCost = 0
		if msg.storage != nil {
			if input, output, cost, err := msg.storage.Usage(context.Background()); err == nil {
				m.tokensSent = input
				m.tokensReceived = output
				m.totalCost = cost
			}
		}
		m.lastToolUseID = ""
		m.historyIdx = -1
		m.historyDraft = ""
		return m, tea.Batch(
			tea.Printf("%s\n", m.renderEntry(session.Entry{Role: session.System, Content: msg.notice})),
			m.awaitSessionEvent(),
		)

	case tea.KeyPressMsg:
		return m.handleKey(msg)

	case session.StatusChanged,
		session.TokenUsage,
		session.TurnStarted,
		session.TurnFinished,
		session.ThinkingDelta,
		session.AssistantDelta,
		session.AssistantMessage,
		session.ToolCallStarted,
		session.ToolOutputDelta,
		session.ToolResult,
		session.VerificationResult,
		session.ApprovalRequest,
		session.ChildRequested,
		session.ChildStarted,
		session.ChildDelta,
		session.ChildCompleted,
		session.ChildFailed,
		session.Error:
		return m.handleSessionEvent(msg.(session.Event))
	}

	// Pass remaining messages to composer
	var cmd tea.Cmd
	m.composer, cmd = m.composer.Update(msg)
	if m.ready {
		m.layout()
	}
	return m, cmd
}

func ifthen[T any](cond bool, a, b T) T {
	if cond {
		return a
	}
	return b
}

func now() int64 { return time.Now().Unix() }
