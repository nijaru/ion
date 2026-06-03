package app

import (
	"context"
	"time"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
	"github.com/nijaru/ion/internal/core"
	"github.com/nijaru/ion/session"
)

const (
	minComposerHeight = 1
	maxComposerHeight = 10
)

type sessionEventMsg struct {
	generation uint64
	event      session.AgentEvent
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

const (
	presetPrimary = core.PresetPrimary
	presetFast    = core.PresetFast
)

type runtimeSwitchedMsg struct {
	switchID      uint64
	runtime       core.Accepted
	previous      core.Handles
	printLines    []string
	replayEntries []session.Entry
	notice        string
	showStatus    bool
}

type TransitionCommittedMsg struct {
	switchID   uint64
	transition core.Transition
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
	preset    core.Preset
	selection core.ProviderSelection
	err       error
}

type modelPickerLoadedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    core.Preset
	items     []pickerItem
	err       error
}

type modelPickerSetupResolvedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    core.Preset
	setup     core.SetupPromptKind
	err       error
}

type setupPromptSavedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    core.Preset
	err       error
}

type settingsCommandMsg struct {
	requestID     uint64
	transition    core.Transition
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
	info session.SessionInfo
}

type sessionPickerLoadedMsg struct {
	requestID uint64
	sessions  []session.SessionInfo
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
	preset   core.Preset
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
	kind         core.SetupPromptKind
	provider     string
	providerName string
	value        string
	preset       core.Preset
	cfg          config.Config
	err          string
	saving       bool
	request      uint64
}


const (
	stateReady      = core.StateReady
	stateIonizing   = core.StateIonizing
	stateStreaming  = core.StateStreaming
	stateWorking    = core.StateWorking
	stateComplete   = core.StateComplete
	stateCancelled  = core.StateCancelled
	stateBlocked    = core.StateBlocked
	stateError      = core.StateError
)


// AppState holds general application and workspace metadata.
type AppState struct {
	Width             int
	Height            int
	Ready             bool
	Workdir           string
	Branch            string
	GitDiff           string
	Version           string
	ActivePreset      core.Preset
	PrintedTranscript bool
}

// ModelState holds the core backend, session, and storage handles.
type ModelState struct {
	Backend              core.Backend
	Session              session.AgentSession
	Storage              session.SessionHandle
	Store                session.SessionStore
	Switcher             core.Switcher
	Config               *config.Config
	Runtime              core.Snapshot
	Checkpoints          *ionworkspace.CheckpointStore
	EventGeneration      uint64
	RuntimeSwitchRequest uint64
	SettingsRequest      uint64
}

// core.SubagentProgress, core.InFlightState, core.ProgressState are aliases for core types.



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
	InFlight core.InFlightState
	Picker   PickerState
	Progress core.ProgressState
	Input    InputState

	// PasteMarkers stores original content for collapsed large pastes.
	// Key is the placeholder text (e.g. "[paste #1 +123 lines]").
	PasteMarkers map[string]pasteMarker
	pasteSeq     int // next paste marker ID

	// Styles (initialized once in New)
	st styles
}

func New(
	b core.Backend,
	s session.SessionHandle,
	store session.SessionStore,
	workdir, branch, version string,
	switcher core.Switcher,
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

	var boot core.Bootstrap
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
		InFlight: core.InFlightState{
			Subagents: make(map[string]*core.SubagentProgress),
		},
		Progress: core.ProgressState{
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
		m.App.ActivePreset = core.PresetFromString(*state.ActivePreset)
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
