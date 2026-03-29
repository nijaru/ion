package app

import (
	"context"
	"fmt"
	"strings"
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

type clearPendingMsg struct {
	action pendingAction
}

type pendingAction int

const (
	pendingActionNone pendingAction = iota
	pendingActionQuitCtrlC
	pendingActionQuitCtrlD
	pendingActionClearEsc
)

const pendingActionTimeout = 1500 * time.Millisecond

type runtimeSwitcher func(context.Context, *config.Config, string) (backend.Backend, session.AgentSession, storage.Session, error)

type runtimeSwitchedMsg struct {
	backend       backend.Backend
	session       session.AgentSession
	storage       storage.Session
	printLines    []string
	replayEntries []session.Entry
	status        string
	notice        string
	showStatus    bool
}

type sessionCompactedMsg struct {
	notice string
}

type sessionCostMsg struct {
	notice string
}

type sessionHelpMsg struct {
	notice string
}

type queuedTurnMsg struct {
	text string
}

type sessionPickerItem struct {
	info storage.SessionInfo
}

type sessionPickerState struct {
	items    []sessionPickerItem
	filtered []sessionPickerItem
	index    int
	query    string
	err      string
}

type pickerPurpose int

const (
	pickerPurposeProvider pickerPurpose = iota
	pickerPurposeModel
	pickerPurposeThinking
)

type pickerItem struct {
	Label   string
	Value   string
	Detail  string
	Group   string
	Tone    pickerTone
	Metrics *pickerMetrics
	Search  []pickerSearchField
}

type pickerMetrics struct {
	Context string
	Input   string
	Output  string
}

type pickerTone int

const (
	pickerToneDefault pickerTone = iota
	pickerToneWarn
)

type pickerState struct {
	title    string
	items    []pickerItem
	filtered []pickerItem
	index    int
	query    string
	purpose  pickerPurpose
	cfg      *config.Config
}

type progressState int

const (
	stateReady progressState = iota
	stateIonizing
	stateStreaming
	stateWorking
	stateComplete
	stateApproval
	stateCancelled
	stateError
)

type turnSummary struct {
	Elapsed time.Duration
	Input   int
	Output  int
	Cost    float64
}

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
	store    storage.Store
	switcher runtimeSwitcher

	// In-flight state — Plane B content
	pending    *session.Entry // streaming agent, active tool, or active subagent
	reasonBuf  string         // accumulates ThinkingDelta
	streamBuf  string         // accumulates AgentDelta (mirrors pending.Content)
	queuedTurn string

	// Approval
	pendingApproval *session.ApprovalRequest

	// Selection overlay
	picker        *pickerState
	sessionPicker *sessionPickerState

	// Progress and status
	progress          progressState
	lastError         string
	thinking          bool
	turnStartedAt     time.Time
	currentTurnInput  int
	currentTurnOutput int
	currentTurnCost   float64
	lastTurnSummary   turnSummary

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
	escPending    bool
	ctrlCPending  bool
	pendingAction pendingAction

	// Storage correlation
	lastToolUseID string

	// Workspace metadata
	status            string
	reasoningEffort   string
	workdir           string
	branch            string
	version           string
	mode              session.Mode
	printedTranscript bool
	startupLines      []string
	startupEntries    []session.Entry

	// Styles (initialized once in New)
	st styles
}

func New(b backend.Backend, s storage.Session, store storage.Store, workdir, branch, version string, switcher runtimeSwitcher) Model {
	ta := textarea.New()
	ta.Placeholder = "Type a message..."
	ta.Prompt = "› "
	ta.ShowLineNumbers = false
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
	ta.MaxHeight = maxComposerHeight
	ta.Focus()
	taStyles := ta.Styles()
	taStyles.Focused.CursorLine = taStyles.Focused.CursorLine.UnsetBackground()
	taStyles.Blurred.CursorLine = taStyles.Blurred.CursorLine.UnsetBackground()
	ta.SetStyles(taStyles)

	st := newStyles()

	spt := spinner.New()
	spt.Spinner = spinner.MiniDot
	spt.Style = st.cyan

	boot := b.Bootstrap()

	m := Model{
		backend:    b,
		session:    b.Session(),
		storage:    s,
		store:      store,
		switcher:   switcher,
		composer:   ta,
		spinner:    spt,
		status:     boot.Status,
		workdir:    workdir,
		branch:     branch,
		version:    version,
		mode:       session.ModeWrite,
		historyIdx: -1,
		st:         st,
	}
	if cfg, err := config.Load(); err == nil {
		m.reasoningEffort = normalizeThinkingValue(cfg.ReasoningEffort)
	} else {
		m.reasoningEffort = config.DefaultReasoningEffort
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

func (m Model) WithStartupLines(lines []string) Model {
	m.startupLines = append([]string(nil), lines...)
	return m
}

func (m Model) WithStartupEntries(entries []session.Entry) Model {
	m.startupEntries = append([]session.Entry(nil), entries...)
	return m
}

func (m Model) WithPrintedTranscript(v bool) Model {
	m.printedTranscript = v
	return m
}

func (m Model) startupPrintLines() []string {
	lines := make([]string, 0, len(m.startupLines)+len(m.startupEntries)+2)
	lines = append(lines, m.startupLines...)
	lines = append(lines, m.headerLine())
	if m.status != "" && !isConfigurationStatus(m.status) {
		lines = append(lines, "")
		lines = append(lines, m.renderStartupStatus(m.status))
	}
	for _, entry := range m.startupEntries {
		lines = append(lines, m.renderEntry(entry))
	}
	return lines
}

func (m Model) renderStartupBlock() string {
	lines := m.startupPrintLines()
	if len(lines) == 0 {
		return ""
	}
	return strings.Join(lines, "\n")
}

func (m Model) renderStartupStatus(status string) string {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return ""
	}

	if isConfigurationStatus(trimmed) {
		return m.st.warn.Render("• " + trimmed)
	}
	return m.st.dim.Render(trimmed)
}

func isConfigurationStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return false
	}
	lower := strings.ToLower(trimmed)
	return trimmed == noProviderConfiguredStatus() ||
		trimmed == noModelConfiguredStatus() ||
		strings.HasPrefix(lower, "provider and model are required")
}

func noProviderConfiguredStatus() string {
	return "No provider configured. Use /provider or Ctrl+P. Set ION_PROVIDER for scripts."
}

func noModelConfiguredStatus() string {
	return "No model configured. Use /model or Ctrl+M. Set ION_MODEL for scripts."
}

func (m Model) configurationStatus() string {
	if m.backend == nil {
		return ""
	}
	if strings.TrimSpace(m.backend.Provider()) == "" {
		return noProviderConfiguredStatus()
	}
	if strings.TrimSpace(m.backend.Model()) == "" {
		return noModelConfiguredStatus()
	}
	return ""
}

func (m Model) runningProgressParts() []string {
	parts := []string{}
	if m.currentTurnInput > 0 {
		parts = append(parts, "↑ "+compactCount(m.currentTurnInput))
	}
	if m.currentTurnOutput > 0 {
		parts = append(parts, "↓ "+compactCount(m.currentTurnOutput))
	}
	if !m.turnStartedAt.IsZero() {
		parts = append(parts, fmt.Sprintf("%ds", int(time.Since(m.turnStartedAt).Seconds())))
	}
	parts = append(parts, "Esc to cancel")
	return parts
}

func (m Model) completedProgressParts() []string {
	parts := []string{}
	if m.lastTurnSummary.Input > 0 {
		parts = append(parts, "↑ "+compactCount(m.lastTurnSummary.Input))
	}
	if m.lastTurnSummary.Output > 0 {
		parts = append(parts, "↓ "+compactCount(m.lastTurnSummary.Output))
	}
	if m.lastTurnSummary.Cost > 0 {
		parts = append(parts, fmt.Sprintf("$%.4f", m.lastTurnSummary.Cost))
	}
	if m.lastTurnSummary.Elapsed > 0 {
		parts = append(parts, fmt.Sprintf("%ds", int(m.lastTurnSummary.Elapsed.Seconds())))
	}
	return parts
}

func compactCount(n int) string {
	if n >= 1000 {
		return fmt.Sprintf("%.1fk", float64(n)/1000.0)
	}
	return fmt.Sprintf("%d", n)
}

func isIdleStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return true
	}
	switch strings.ToLower(trimmed) {
	case "ready", "connected via canto", "connected via acp":
		return true
	default:
		return false
	}
}

func (m Model) runtimeHeaderLine(_ backend.Backend) string {
	version := strings.TrimSpace(m.version)
	if version == "" {
		version = "v0.0.0"
	}
	return "ion " + version
}

func (m Model) Init() tea.Cmd {
	m.session.SetMode(m.mode)
	return tea.Batch(
		textarea.Blink,
		m.spinner.Tick,
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

	case clearPendingMsg:
		if msg.action == m.pendingAction {
			m.clearPendingAction()
		}
		return m, nil

	case runtimeSwitchedMsg:
		m.backend = msg.backend
		m.session = msg.session
		m.storage = msg.storage
		m.picker = nil
		m.sessionPicker = nil
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
		cmds := make([]tea.Cmd, 0, 5)
		if len(msg.printLines) > 0 {
			cmds = append(cmds, printLinesCmd(msg.printLines...))
		}
		if len(msg.replayEntries) > 0 {
			cmds = append(cmds, m.printEntries(msg.replayEntries...))
		}
		if strings.TrimSpace(msg.notice) != "" {
			cmds = append(cmds, m.printEntries(session.Entry{Role: session.System, Content: msg.notice}))
		}
		if msg.showStatus && strings.TrimSpace(msg.status) != "" && !isConfigurationStatus(msg.status) {
			cmds = append(cmds, m.printEntries(session.Entry{Role: session.System, Content: msg.status}))
		}
		cmds = append(cmds, m.awaitSessionEvent())
		return m, tea.Sequence(cmds...)

	case sessionCompactedMsg:
		return m, m.printEntries(session.Entry{Role: session.System, Content: msg.notice})

	case sessionCostMsg:
		return m, m.printEntries(session.Entry{Role: session.System, Content: msg.notice})

	case sessionHelpMsg:
		return m, m.printEntries(session.Entry{Role: session.System, Content: msg.notice})

	case queuedTurnMsg:
		next, cmd := m.submitText(msg.text)
		return next, cmd

	case tea.KeyPressMsg:
		return m.handleKey(msg)

	case session.StatusChanged,
		session.TokenUsage,
		session.TurnStarted,
		session.TurnFinished,
		session.ThinkingDelta,
		session.AgentDelta,
		session.AgentMessage,
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

func printLinesCmd(lines ...string) tea.Cmd {
	filtered := make([]string, 0, len(lines))
	for _, line := range lines {
		filtered = append(filtered, line)
	}
	if len(filtered) == 0 {
		return nil
	}
	return tea.Printf("%s\n", strings.Join(filtered, "\n"))
}

func printEntriesCmd(m Model, entries ...session.Entry) tea.Cmd {
	if len(entries) == 0 {
		return nil
	}
	lines := make([]string, 0, len(entries))
	for _, entry := range entries {
		lines = append(lines, m.renderEntry(entry))
	}
	return printLinesCmd(lines...)
}

func (m *Model) printEntries(entries ...session.Entry) tea.Cmd {
	if len(entries) == 0 {
		return nil
	}
	lines := make([]string, 0, len(entries)+1)
	if !m.printedTranscript {
		lines = append(lines, "")
		m.printedTranscript = true
	}
	for _, entry := range entries {
		lines = append(lines, m.renderEntry(entry))
	}
	return printLinesCmd(lines...)
}

func (m *Model) clearPendingAction() {
	m.escPending = false
	m.ctrlCPending = false
	m.pendingAction = pendingActionNone
}

func (m *Model) armPendingAction(action pendingAction) tea.Cmd {
	m.pendingAction = action
	switch action {
	case pendingActionClearEsc:
		m.escPending = true
		m.ctrlCPending = false
	case pendingActionQuitCtrlC, pendingActionQuitCtrlD:
		m.ctrlCPending = true
		m.escPending = false
	default:
		m.clearPendingAction()
		return nil
	}
	return tea.Tick(pendingActionTimeout, func(time.Time) tea.Msg {
		return clearPendingMsg{action: action}
	})
}

func (m Model) pendingActionStatus() string {
	switch m.pendingAction {
	case pendingActionQuitCtrlC:
		return "Press Ctrl+C again to quit"
	case pendingActionQuitCtrlD:
		return "Press Ctrl+D again to quit"
	case pendingActionClearEsc:
		return "Press Esc again to clear input"
	default:
		return ""
	}
}
