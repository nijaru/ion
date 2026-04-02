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

type modelPreset string

const (
	presetPrimary modelPreset = "primary"
	presetFast    modelPreset = "fast"
)

type runtimeSwitcher func(context.Context, *config.Config, string) (backend.Backend, session.AgentSession, storage.Session, error)

type runtimeSwitchedMsg struct {
	cfg           *config.Config
	backend       backend.Backend
	session       session.AgentSession
	storage       storage.Session
	preset        modelPreset
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

type pickerOverlayState struct {
	title    string
	items    []pickerItem
	filtered []pickerItem
	index    int
	query    string
	purpose  pickerPurpose
	cfg      *config.Config
}

type progressMode int

const (
	stateReady progressMode = iota
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

// AppState holds general application and workspace metadata.
type AppState struct {
	Width             int
	Height            int
	Ready             bool
	Workdir           string
	Branch            string
	Version           string
	ActivePreset      modelPreset
	PrintedTranscript bool
	StartupLines      []string
	StartupEntries    []session.Entry
}

// ModelState holds the core backend, session, and storage handles.
type ModelState struct {
	Backend  backend.Backend
	Session  session.AgentSession
	Storage  storage.Session
	Store    storage.Store
	Switcher runtimeSwitcher
}

// InFlightState holds data for the currently active turn or streaming response.
type InFlightState struct {
	Pending     *session.Entry // streaming agent, active tool, or active subagent
	ReasonBuf   string         // accumulates ThinkingDelta
	StreamBuf   string         // accumulates AgentDelta (mirrors pending.Content)
	QueuedTurns []string       // follow-up turns queued during agent work
	Thinking    bool
}

// ApprovalState holds pending approval requests.
type ApprovalState struct {
	Pending *session.ApprovalRequest
}

// PickerState holds state for the various overlay pickers.
type PickerState struct {
	Overlay *pickerOverlayState
	Session *sessionPickerState
}

// ProgressState holds turn-level metrics and overall progress status.
type ProgressState struct {
	Mode              progressMode
	LastError         string
	Status            string
	ReasoningEffort   string
	TurnStartedAt     time.Time
	CurrentTurnInput  int
	CurrentTurnOutput int
	CurrentTurnCost   float64
	LastTurnSummary   turnSummary
	TokensSent        int
	TokensReceived    int
	TotalCost         float64
	LastToolUseID     string
}

// InputState holds state for the composer, history, and double-tap tracking.
type InputState struct {
	Composer     textarea.Model
	Spinner      spinner.Model
	History      []string
	HistoryIdx   int
	HistoryDraft string
	EscPending   bool
	CtrlCPending bool
	Pending      pendingAction
}

// pasteMarker stores original content for a collapsed large paste.
type pasteMarker struct {
	placeholder string // what's shown in textarea, e.g. "[paste #1 +123 lines]"
	content     string // original paste content
}

// Model is the Bubble Tea model for the ion TUI.
type Model struct {
	App      AppState
	Model    ModelState
	InFlight InFlightState
	Approval ApprovalState
	Picker   PickerState
	Progress ProgressState
	Input    InputState
	Mode     session.Mode

	// PasteMarkers stores original content for collapsed large pastes.
	// Key is the placeholder text (e.g. "[paste #1 +123 lines]").
	PasteMarkers map[string]pasteMarker
	pasteSeq     int // next paste marker ID

	// Styles (initialized once in New)
	st styles
}

func New(
	b backend.Backend,
	s storage.Session,
	store storage.Store,
	workdir, branch, version string,
	switcher runtimeSwitcher,
) Model {
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
		App: AppState{
			Workdir:      workdir,
			Branch:       branch,
			Version:      version,
			ActivePreset: presetPrimary,
		},
		Model: ModelState{
			Backend:  b,
			Session:  b.Session(),
			Storage:  s,
			Store:    store,
			Switcher: switcher,
		},
		Progress: ProgressState{
			Status: boot.Status,
		},
		Input: InputState{
			Composer:   ta,
			Spinner:    spt,
			HistoryIdx: -1,
		},
		Mode:         initialMode(boot),
		PasteMarkers: make(map[string]pasteMarker),
		st:           st,
	}

	if cfg, err := config.Load(); err == nil {
		m.Progress.ReasoningEffort = normalizeThinkingValue(cfg.ReasoningEffort)
	} else {
		m.Progress.ReasoningEffort = config.DefaultReasoningEffort
	}

	if s != nil {
		if input, output, cost, err := s.Usage(context.Background()); err == nil {
			m.Progress.TokensSent = input
			m.Progress.TokensReceived = output
			m.Progress.TotalCost = cost
		}
	}

	return m
}

func (m Model) WithStartupLines(lines []string) Model {
	m.App.StartupLines = append([]string(nil), lines...)
	return m
}

func (m Model) WithStartupEntries(entries []session.Entry) Model {
	m.App.StartupEntries = append([]session.Entry(nil), entries...)
	return m
}

func (m Model) WithPrintedTranscript(v bool) Model {
	m.App.PrintedTranscript = v
	return m
}

func (m Model) startupPrintLines() []string {
	lines := make([]string, 0, len(m.App.StartupLines)+len(m.App.StartupEntries)+2)
	lines = append(lines, m.App.StartupLines...)
	lines = append(lines, m.headerLine())
	if m.Progress.Status != "" && !isConfigurationStatus(m.Progress.Status) {
		lines = append(lines, "")
		lines = append(lines, m.renderStartupStatus(m.Progress.Status))
	}
	for _, entry := range m.App.StartupEntries {
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

func (m Model) configurationStatus() string {
	if m.Model.Backend == nil {
		return ""
	}
	if strings.TrimSpace(m.Model.Backend.Provider()) == "" {
		return noProviderConfiguredStatus()
	}
	if strings.TrimSpace(m.Model.Backend.Model()) == "" {
		return noModelConfiguredStatus()
	}
	return ""
}

func (m Model) runningProgressParts() []string {
	parts := []string{}
	if m.Progress.CurrentTurnInput > 0 {
		parts = append(parts, "↑ "+compactCount(m.Progress.CurrentTurnInput))
	}
	if m.Progress.CurrentTurnOutput > 0 {
		parts = append(parts, "↓ "+compactCount(m.Progress.CurrentTurnOutput))
	}
	if !m.Progress.TurnStartedAt.IsZero() {
		parts = append(parts, fmt.Sprintf("%ds", int(time.Since(m.Progress.TurnStartedAt).Seconds())))
	}
	parts = append(parts, "Esc to cancel")
	return parts
}

func (m Model) completedProgressParts() []string {
	parts := []string{}
	if m.Progress.LastTurnSummary.Input > 0 {
		parts = append(parts, "↑ "+compactCount(m.Progress.LastTurnSummary.Input))
	}
	if m.Progress.LastTurnSummary.Output > 0 {
		parts = append(parts, "↓ "+compactCount(m.Progress.LastTurnSummary.Output))
	}
	if m.Progress.LastTurnSummary.Cost > 0 {
		parts = append(parts, fmt.Sprintf("$%.4f", m.Progress.LastTurnSummary.Cost))
	}
	if m.Progress.LastTurnSummary.Elapsed > 0 {
		parts = append(parts, fmt.Sprintf("%ds", int(m.Progress.LastTurnSummary.Elapsed.Seconds())))
	}
	return parts
}

func (m Model) runtimeHeaderLine(_ backend.Backend) string {
	version := strings.TrimSpace(m.App.Version)
	if version == "" {
		version = "v0.0.0"
	}
	return "ion " + version
}

func (m Model) Init() tea.Cmd {
	m.Model.Session.SetMode(m.Mode)
	m.Model.Session.SetAutoApprove(m.Mode == session.ModeYolo)
	return tea.Batch(
		textarea.Blink,
		m.Input.Spinner.Tick,
		m.awaitSessionEvent(),
	)
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case spinner.TickMsg:
		var cmd tea.Cmd
		m.Input.Spinner, cmd = m.Input.Spinner.Update(msg)
		return m, cmd

	case tea.WindowSizeMsg:
		m.App.Ready = true
		m.App.Width = msg.Width
		m.App.Height = msg.Height
		m.layout()
		return m, nil

	case streamClosedMsg:
		return m, nil

	case clearPendingMsg:
		if msg.action == m.Input.Pending {
			m.clearPendingAction()
		}
		return m, nil

	case runtimeSwitchedMsg:
		if msg.preset == "" {
			m.App.ActivePreset = presetPrimary
		} else {
			m.App.ActivePreset = msg.preset
		}
		m.Model.Backend = msg.backend
		m.Model.Session = msg.session
		m.Model.Storage = msg.storage
		m.Picker.Overlay = nil
		m.Picker.Session = nil
		m.Progress.Status = msg.status
		if msg.cfg != nil {
			m.Progress.ReasoningEffort = normalizeThinkingValue(msg.cfg.ReasoningEffort)
		}
		if msg.storage != nil {
			meta := msg.storage.Meta()
			m.App.Branch = meta.Branch
		}
		m.InFlight.Pending = nil
		m.Approval.Pending = nil
		m.InFlight.QueuedTurns = nil
		m.InFlight.ReasonBuf = ""
		m.InFlight.StreamBuf = ""
		m.Progress.Mode = stateReady
		m.Progress.LastError = ""
		m.Progress.LastTurnSummary = turnSummary{}
		m.InFlight.Thinking = false
		m.Input.CtrlCPending = false
		m.Input.EscPending = false
		m.Progress.TokensSent = 0
		m.Progress.TokensReceived = 0
		m.Progress.TotalCost = 0
		if msg.storage != nil {
			if input, output, cost, err := msg.storage.Usage(context.Background()); err == nil {
				m.Progress.TokensSent = input
				m.Progress.TokensReceived = output
				m.Progress.TotalCost = cost
			}
		}
		m.Progress.LastToolUseID = ""
		m.Input.HistoryIdx = -1
		m.Input.HistoryDraft = ""
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

	case tea.PasteMsg:
		return m.handlePaste(msg)

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
	m.Input.Composer, cmd = m.Input.Composer.Update(msg)
	if m.App.Ready {
		m.layout()
	}
	return m, cmd
}
