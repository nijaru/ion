package app

import (
	"context"
	"time"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/runtimecontroller"
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

type fileReferenceCompletionMsg struct {
	requestID uint64
	text      string
	start     int
	token     string
	matches   []fileReferenceMatch
	apply     bool
}

type pendingAction int

const (
	pendingActionNone pendingAction = iota
	pendingActionQuitCtrlC
	pendingActionQuitCtrlD
)

const pendingActionTimeout = 1500 * time.Millisecond

type modelPreset = runtimecontroller.Preset

const (
	presetPrimary = runtimecontroller.PresetPrimary
	presetFast    = runtimecontroller.PresetFast
)

type runtimeSwitcher = runtimecontroller.Switcher

type runtimeHandles = runtimecontroller.Handles

type runtimeSnapshot = runtimecontroller.Snapshot

type runtimeTransition = runtimecontroller.Transition

type acceptedRuntime = runtimecontroller.Accepted

type runtimeSwitchedMsg struct {
	switchID      uint64
	runtime       acceptedRuntime
	previous      runtimeHandles
	printLines    []string
	replayEntries []session.Entry
	notice        string
	showStatus    bool
}

type runtimeTransitionCommittedMsg struct {
	switchID   uint64
	transition runtimeTransition
	notice     session.Entry
	err        error
}

type runtimeSwitchErrorMsg struct {
	switchID uint64
	err      error
}

type resumeSessionSelectedMsg struct {
	switchID  uint64
	sessionID string
	cfg       *config.Config
}

type providerSelectionResolvedMsg struct {
	requestID uint64
	provider  string
	preset    modelPreset
	selection providerSelection
	err       error
}

type modelPickerLoadedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    modelPreset
	items     []pickerItem
	err       error
}

type modelPickerSetupResolvedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    modelPreset
	setup     setupPromptKind
	err       error
}

type setupPromptSavedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    modelPreset
	err       error
}

type settingsCommandMsg struct {
	requestID     uint64
	transition    runtimeTransition
	hasTransition bool
	notice        string
	err           error
}

type sessionCompactedMsg struct {
	notice string
}

type sessionCostMsg struct {
	notice string
}

type sessionUsageLoadedMsg struct {
	generation uint64
	input      int
	output     int
	cost       float64
	err        error
}

type localEntriesMsg struct {
	entries []session.Entry
}

type terminalCommitLinesMsg struct {
	lines []string
}

type gitDiffStatsMsg struct {
	workdir string
	stats   string
}

type queuedTurnMsg struct {
	text               string
	rearmSessionEvents bool
}

type turnSubmitResultMsg struct {
	text  string
	draft string
	err   error
	rearm bool
}

type steeringResultMsg struct {
	text   string
	result session.SteeringResult
	err    error
}

type followUpResultMsg struct {
	text               string
	priorFollowUpCount int
	result             session.QueuedInputResult
	err                error
}

type queuedInputClearResultMsg struct {
	err error
}

type turnCancelResultMsg struct {
	err error
}

type sessionPickerItem struct {
	info storage.SessionInfo
}

type sessionPickerLoadedMsg struct {
	requestID uint64
	sessions  []storage.SessionInfo
	err       error
}

type sessionPickerState struct {
	items    []sessionPickerItem
	filtered []sessionPickerItem
	index    int
	query    string
	err      string
	loading  bool
	request  uint64
}

type pickerPurpose int

const (
	pickerPurposeProvider pickerPurpose = iota
	pickerPurposeModel
	pickerPurposeThinking
	pickerPurposeCommand
	pickerPurposeSettings
)

type setupPromptKind int

const (
	setupPromptAPIKey setupPromptKind = iota + 1
	setupPromptEndpoint
)

type pickerItem struct {
	Label       string
	Value       string
	Detail      string
	Group       string
	Tone        pickerTone
	Metrics     *pickerMetrics
	Search      []pickerSearchField
	SettingName string
	CurrentVal  string
	Desc        string
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
	setup    bool
}

type completionState struct {
	items []completionItem
}

type completionItem struct {
	Label  string
	Detail string
}

type setupPromptState struct {
	kind         setupPromptKind
	provider     string
	providerName string
	value        string
	preset       modelPreset
	cfg          config.Config
	err          string
	saving       bool
	request      uint64
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
	Runtime              runtimeSnapshot
	Checkpoints          *ionworkspace.CheckpointStore
	EventGeneration      uint64
	RuntimeSwitchRequest uint64
	SettingsRequest      uint64
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
	Pending                 *session.Entry               // streaming agent, active tool, or active subagent
	PendingTools            map[string]*session.Entry    // active tool calls by backend tool ID
	Subagents               map[string]*SubagentProgress // active child agents by ID
	ReasonBuf               string                       // accumulates ThinkingDelta
	StreamBuf               string                       // non-empty while AgentDelta content is active
	StreamChunks            []string                     // full AgentDelta content without per-event string copies
	QueuedSteering          []string                     // steering messages queued during agent work
	QueuedTurns             []string                     // follow-up turns queued during agent work
	QueuedTurnsBackendOwned bool
	Thinking                bool
	Canceling               bool
	AgentCommitted          bool // true once AgentMessage owns the turn transcript
	DrainUntilTurnStarted   bool
	DrainStartedAt          time.Time
}

// PickerState holds state for the various overlay pickers.
type PickerState struct {
	Overlay                  *pickerOverlayState
	Session                  *sessionPickerState
	Setup                    *setupPromptState
	ModelLoadRequest         uint64
	SessionLoadRequest       uint64
	ProviderSelectionRequest uint64
	SetupSaveRequest         uint64
	OverlayClosedAt          time.Time
	PreStartupMode           bool
	SelectedSessionID        string
}

// ProgressState holds turn-level metrics and overall progress status.
type ProgressState struct {
	Mode              progressMode
	LastError         string
	Status            string
	StatusUpdatedAt   time.Time
	LocalStatus       string
	LocalStatusAt     time.Time
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
	Composer              textarea.Model
	Completion            *completionState
	FileCompletionRequest uint64
	Spinner               spinner.Model
	History               []string
	HistoryIdx            int
	HistoryDraft          string
	Pending               pendingAction
	PrintHoldUntil        time.Time
	PrintHoldDelay        time.Duration
	DelayNextEnter        bool
	DeferredEnter         bool
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
	ta.Prompt = ""
	ta.ShowLineNumbers = false
	ta.DynamicHeight = true
	ta.MinHeight = minComposerHeight
	ta.MaxHeight = maxComposerHeight
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(max(1, 80-composerPromptWidth()))
	ta.Focus()
	taStyles := ta.Styles()
	taStyles.Focused.CursorLine = taStyles.Focused.CursorLine.UnsetBackground()
	taStyles.Blurred.CursorLine = taStyles.Blurred.CursorLine.UnsetBackground()
	ta.SetStyles(taStyles)

	st := newStyles()

	spt := spinner.New()
	spt.Spinner = spinner.MiniDot
	spt.Style = st.cyan

	var boot backend.Bootstrap
	var sess session.AgentSession
	if b != nil {
		boot = b.Bootstrap()
		sess = b.Session()
	}
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
			Session:     sess,
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

	if cfg, err := config.Load(); err == nil {
		m.Model.Config = cfg
		m.progressReducer().setReasoningEffort(normalizeThinkingValue(cfg.ReasoningEffort))
	} else {
		m.progressReducer().setReasoningEffort(config.DefaultReasoningEffort)
	}

	if s != nil {
		if input, output, cost, err := s.Usage(context.Background()); err == nil {
			m.progressReducer().applySessionUsage(input, output, cost)
		}
	}
	m.loadInputHistory(context.Background())

	return m
}



func (m Model) Init() tea.Cmd {
	cmds := []tea.Cmd{
		textarea.Blink,
		m.Input.Spinner.Tick,
		loadGitDiffStats(m.App.Workdir),
		m.startupPickerCmd(),
	}
	if m.Model.Session != nil {
		cmds = append(cmds, m.awaitSessionEvent())
	}
	return tea.Batch(cmds...)
}

func (m Model) SelectedSessionID() string {
	return m.Picker.SelectedSessionID
}
