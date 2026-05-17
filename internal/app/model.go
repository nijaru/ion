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
	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

const (
	minComposerHeight = 1
	maxComposerHeight = 10
)

type sessionEventMsg struct {
	generation uint64
	event      session.Event
}

type streamClosedMsg struct {
	generation uint64
}

type clearPendingMsg struct {
	action pendingAction
}

type deferredEnterMsg struct{}

type pendingAction int

const (
	pendingActionNone pendingAction = iota
	pendingActionQuitCtrlC
	pendingActionQuitCtrlD
)

const pendingActionTimeout = 1500 * time.Millisecond

type modelPreset string

const (
	presetPrimary modelPreset = "primary"
	presetFast    modelPreset = "fast"
)

type runtimeSwitcher func(context.Context, *config.Config, string) (backend.Backend, session.AgentSession, storage.Session, error)

type runtimeSwitchedMsg struct {
	switchID      uint64
	cfg           *config.Config
	runtimeCfg    *config.Config
	backend       backend.Backend
	session       session.AgentSession
	storage       storage.Session
	oldSession    session.AgentSession
	oldStorage    storage.Session
	preset        modelPreset
	printLines    []string
	replayEntries []session.Entry
	status        string
	notice        string
	showStatus    bool
}

type runtimeSwitchErrorMsg struct {
	switchID uint64
	err      error
}

type modelPickerLoadedMsg struct {
	requestID uint64
	cfg       config.Config
	items     []pickerItem
	err       error
}

type sessionCompactedMsg struct {
	notice string
}

type sessionCostMsg struct {
	notice string
}

type localEntriesMsg struct {
	entries []session.Entry
}

type gitDiffStatsMsg struct {
	workdir string
	stats   string
}

type queuedTurnMsg struct {
	text               string
	rearmSessionEvents bool
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
	pickerPurposeCommand
)

type setupPromptKind int

const (
	setupPromptAPIKey setupPromptKind = iota + 1
	setupPromptEndpoint
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
	preset   modelPreset
	cfg      *config.Config
	loading  bool
	err      string
	request  uint64
}

type setupPromptState struct {
	kind         setupPromptKind
	provider     string
	providerName string
	value        string
	preset       modelPreset
	cfg          config.Config
	err          string
}

type progressMode int

const (
	stateReady progressMode = iota
	stateIonizing
	stateStreaming
	stateWorking
	stateComplete
	stateCancelled
	stateBlocked
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
	GitDiff           string
	Version           string
	ActivePreset      modelPreset
	Sandbox           string
	PrintedTranscript bool
}

// ModelState holds the core backend, session, and storage handles.
type ModelState struct {
	Backend              backend.Backend
	Session              session.AgentSession
	Storage              storage.Session
	Store                storage.Store
	Switcher             runtimeSwitcher
	Config               *config.Config
	Checkpoints          *ionworkspace.CheckpointStore
	EventGeneration      uint64
	RuntimeSwitchRequest uint64
}

// SubagentProgress tracks the ephemeral state of a background worker.
type SubagentProgress struct {
	ID        string
	Name      string
	Intent    string
	Status    string
	Output    string
	Reasoning string
}

// InFlightState holds data for the currently active turn or streaming response.
type InFlightState struct {
	Pending               *session.Entry               // streaming agent, active tool, or active subagent
	PendingTools          map[string]*session.Entry    // active tool calls by backend tool ID
	Subagents             map[string]*SubagentProgress // active child agents by ID
	ReasonBuf             string                       // accumulates ThinkingDelta
	StreamBuf             string                       // accumulates AgentDelta (mirrors pending.Content)
	QueuedTurns           []string                     // follow-up turns queued during agent work
	Thinking              bool
	AgentCommitted        bool // true once AgentMessage owns the turn transcript
	DrainUntilTurnStarted bool
}

// PickerState holds state for the various overlay pickers.
type PickerState struct {
	Overlay          *pickerOverlayState
	Session          *sessionPickerState
	Setup            *setupPromptState
	ModelLoadRequest uint64
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
	BudgetStopReason  string
	Compacting        bool
	LastTurnSummary   turnSummary
	TokensSent        int
	TokensReceived    int
	ContextTokens     int
	TotalCost         float64
	LastToolUseID     string
}

// InputState holds state for the composer, history, and double-tap tracking.
type InputState struct {
	Composer       textarea.Model
	Spinner        spinner.Model
	History        []string
	HistoryIdx     int
	HistoryDraft   string
	Pending        pendingAction
	PrintHoldUntil time.Time
	PrintHoldDelay time.Duration
	DelayNextEnter bool
	DeferredEnter  bool
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
	Picker   PickerState
	Progress ProgressState
	Input    InputState

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
	ta.DynamicHeight = true
	ta.MinHeight = minComposerHeight
	ta.MaxHeight = maxComposerHeight
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
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
	var checkpoints *ionworkspace.CheckpointStore
	if checkpointPath, err := ionworkspace.DefaultCheckpointPath(); err == nil {
		checkpoints = ionworkspace.NewCheckpointStore(checkpointPath)
	}

	m := Model{
		App: AppState{
			Workdir:      workdir,
			Branch:       branch,
			Version:      version,
			ActivePreset: presetPrimary,
		},
		Model: ModelState{
			Backend:     b,
			Session:     b.Session(),
			Storage:     s,
			Store:       store,
			Switcher:    switcher,
			Checkpoints: checkpoints,
		},
		InFlight: InFlightState{
			Subagents: make(map[string]*SubagentProgress),
		},
		Progress: ProgressState{
			Status: boot.Status,
		},
		Input: InputState{
			Composer:   ta,
			Spinner:    spt,
			HistoryIdx: -1,
		},
		PasteMarkers: make(map[string]pasteMarker),
		st:           st,
	}

	if state, err := config.LoadState(); err == nil && state.ActivePreset != nil {
		m.App.ActivePreset = modelPresetFromString(*state.ActivePreset)
	}
	m.App.Sandbox = backendSandboxSummary(b)

	if cfg, err := config.Load(); err == nil {
		m.Model.Config = cfg
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
	m.loadInputHistory(context.Background())

	return m
}

func (m Model) WithPrintedTranscript(v bool) Model {
	m.App.PrintedTranscript = v
	return m
}

func (m Model) WithConfig(cfg *config.Config) Model {
	return m.WithConfigForRuntime(cfg, cfg)
}

func (m Model) WithConfigForRuntime(cfg, runtimeCfg *config.Config) Model {
	return m.WithConfigForRuntimePreset(cfg, runtimeCfg, m.activePreset().String())
}

func (m Model) WithConfigForRuntimePreset(
	cfg, runtimeCfg *config.Config,
	preset string,
) Model {
	if cfg == nil {
		return m
	}
	snapshot := newRuntimeSnapshot(cfg, runtimeCfg, modelPresetFromString(preset), "")
	m.applyRuntimeSnapshot(snapshot)
	return m
}

func (m Model) WithActivePreset(value string) Model {
	m.App.ActivePreset = modelPresetFromString(value)
	return m
}

func (m Model) WithSessionPicker() Model {
	m, _ = m.openSessionPicker()
	return m
}

func (m Model) WithProviderPicker() Model {
	m, _ = m.openProviderPicker()
	return m
}

func (m Model) WithCheckpointStore(store *ionworkspace.CheckpointStore) Model {
	m.Model.Checkpoints = store
	return m
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
		parts = append(
			parts,
			fmt.Sprintf("%ds", int(time.Since(m.Progress.TurnStartedAt).Seconds())),
		)
	}
	if m.Model.Config != nil && m.Model.Config.MaxTurnCost > 0 {
		parts = append(
			parts,
			fmt.Sprintf("$%.4f/$%.4f", m.Progress.CurrentTurnCost, m.Model.Config.MaxTurnCost),
		)
	}
	parts = append(parts, "Esc/Ctrl+C to cancel")
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

func (m Model) costBudgetLabel(cost float64) string {
	if m.Model.Config == nil || m.Model.Config.MaxSessionCost <= 0 {
		if cost <= 0 {
			return ""
		}
		return fmt.Sprintf("$%.3f", cost)
	}
	return fmt.Sprintf("$%.3f/$%.3f", cost, m.Model.Config.MaxSessionCost)
}

func (m Model) configuredBudgetStopReason() string {
	if m.Model.Config == nil {
		return ""
	}
	if m.Model.Config.MaxTurnCost > 0 && m.Progress.CurrentTurnCost >= m.Model.Config.MaxTurnCost {
		return fmt.Sprintf(
			"turn cost limit reached ($%.6f / $%.6f)",
			m.Progress.CurrentTurnCost,
			m.Model.Config.MaxTurnCost,
		)
	}
	return m.configuredSessionBudgetStopReason()
}

func (m Model) configuredSessionBudgetStopReason() string {
	if m.Model.Config == nil {
		return ""
	}
	if m.Model.Config.MaxSessionCost > 0 && m.Progress.TotalCost >= m.Model.Config.MaxSessionCost {
		return fmt.Sprintf(
			"session cost limit reached ($%.6f / $%.6f)",
			m.Progress.TotalCost,
			m.Model.Config.MaxSessionCost,
		)
	}
	return ""
}

type providerLimitError struct {
	reason string
	label  string
	raw    string
}

func classifyProviderLimitError(err error) (providerLimitError, bool) {
	if err == nil {
		return providerLimitError{}, false
	}
	raw := strings.TrimSpace(err.Error())
	if raw == "" {
		return providerLimitError{}, false
	}
	lower := strings.ToLower(raw)
	for _, marker := range []string{
		"context_length_exceeded",
		"context length",
		"maximum context",
		"max context",
		"token limit",
		"too many tokens",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "context_limit",
				label:  "API context limit",
				raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"insufficient_quota",
		"usage limit",
		"quota",
		"billing",
		"credit",
		"credits",
		"balance",
		"spend limit",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "quota_limit",
				label:  "API quota or usage limit",
				raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"status code: 429",
		" 429 ",
		"too many requests",
		"rate limit",
		"rate_limit",
		"requests per",
		"tokens per",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "rate_limit",
				label:  "API rate limit",
				raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"resource_exhausted",
		"overloaded",
		"capacity",
		"temporarily unavailable",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "provider_capacity",
				label:  "Provider capacity limit",
				raw:    raw,
			}, true
		}
	}
	return providerLimitError{}, false
}

func (e providerLimitError) display() string {
	return e.label + ": " + e.raw
}

func (m Model) routingDecision(decision, reason, stopReason string) storage.RoutingDecision {
	provider := ""
	model := ""
	if m.Model.Backend != nil {
		provider = m.Model.Backend.Provider()
		model = m.Model.Backend.Model()
	}
	var maxSessionCost, maxTurnCost float64
	if m.Model.Config != nil {
		maxSessionCost = m.Model.Config.MaxSessionCost
		maxTurnCost = m.Model.Config.MaxTurnCost
	}
	return storage.RoutingDecision{
		Type:           "routing_decision",
		Decision:       decision,
		Reason:         reason,
		ModelSlot:      m.activePreset().String(),
		Provider:       provider,
		Model:          model,
		Reasoning:      normalizeThinkingValue(m.Progress.ReasoningEffort),
		MaxSessionCost: maxSessionCost,
		MaxTurnCost:    maxTurnCost,
		SessionCost:    m.Progress.TotalCost,
		TurnCost:       m.Progress.CurrentTurnCost,
		StopReason:     stopReason,
		TS:             now(),
	}
}

func (m Model) runtimeHeaderLine(_ backend.Backend) string {
	version := strings.TrimSpace(m.App.Version)
	if version == "" {
		version = "v0.0.0"
	}
	return "ion " + version
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(
		textarea.Blink,
		m.Input.Spinner.Tick,
		m.awaitSessionEvent(),
		loadGitDiffStats(m.App.Workdir),
	)
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case spinner.TickMsg:
		var cmd tea.Cmd
		m.Input.Spinner, cmd = m.Input.Spinner.Update(msg)
		return m, cmd

	case tea.WindowSizeMsg:
		return m.handleWindowSize(msg)

	case sessionEventMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil
		}
		return m.handleSessionEvent(msg.event)

	case streamClosedMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil
		}
		return m.handleStreamClosed()

	case clearPendingMsg:
		if msg.action == m.Input.Pending {
			m.clearPendingAction()
		}
		return m, nil

	case deferredEnterMsg:
		return m.handleDeferredEnter()

	case runtimeSwitchedMsg:
		return m.handleRuntimeSwitched(msg)

	case runtimeSwitchErrorMsg:
		return m.handleRuntimeSwitchError(msg)

	case modelPickerLoadedMsg:
		return m.handleModelPickerLoaded(msg)

	case sessionCompactedMsg:
		return m.handleSessionCompacted(msg)

	case sessionCostMsg:
		return m.handleSessionCost(msg)

	case gitDiffStatsMsg:
		return m.handleGitDiffStats(msg)

	case externalEditorFinishedMsg:
		return m.handleExternalEditorFinished(msg)

	case localErrorMsg:
		return m.handleLocalError(msg.err)

	case localEntriesMsg:
		return m.handleLocalEntries(msg)

	case queuedTurnMsg:
		return m.handleQueuedTurn(msg)

	case tea.PasteMsg:
		if m.Picker.Session != nil {
			return m.handleSessionPickerPaste(msg)
		}
		if m.Picker.Setup != nil {
			return m.handleSetupPromptPaste(msg)
		}
		if m.Picker.Overlay != nil {
			return m.handlePickerPaste(msg)
		}
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
		session.ChildRequested,
		session.ChildStarted,
		session.ChildDelta,
		session.ChildCompleted,
		session.ChildBlocked,
		session.ChildFailed,
		session.Error:
		return m.handleSessionEvent(msg.(session.Event))
	}

	// Pass remaining messages to composer
	return m, m.updateComposer(msg)
}
