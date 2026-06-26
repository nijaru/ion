package app

import (
	"context"
	"time"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	
	ionclipboard "github.com/nijaru/ion/internal/clipboard"
	"github.com/nijaru/ion/internal/gitwatch"
	"github.com/nijaru/ion/internal/runtime"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
	"github.com/nijaru/ion/session"
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
type runtimeSwitchedMsg struct {
	switchID      uint64
	runtime       Accepted
	previous      Handles
	printLines    []string
	replayEntries []session.Entry
	notice        string
	showStatus    bool
}

type TransitionCommittedMsg struct {
	switchID   uint64
	transition Transition
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

type allModelsLoadedMsg struct {
	requestID uint64
	items     []pickerItem // All models from all providers, with Provider field set
	err       error
}

type modelPickerLoadedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    Preset
	items     []pickerItem
	err       error
}

type modelPickerSetupResolvedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    Preset
	setup     SetupPromptKind
	err       error
}

type setupPromptSavedMsg struct {
	requestID uint64
	cfg       config.Config
	preset    Preset
	err       error
}

type settingsCommandMsg struct {
	requestID     uint64
	transition    Transition
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

type gitBranchChangedMsg struct {
	branch string
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
	result struct{}
	err    error
}

type followUpResultMsg struct {
	text               string
	priorFollowUpCount int
	result             struct{}
	err                error
}

type queuedInputClearResultMsg struct {
	err error
}

type turnCancelResultMsg struct {
	err error
}

type sessionPickerItem struct {
	info session.SessionInfoEntry
}

type sessionPickerLoadedMsg struct {
	requestID uint64
	sessions  []session.SessionInfoEntry
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
	namedOnly bool // Pi parity: filter to named sessions only
	sortMode sessionSortMode // Pi parity: sort mode for session list
}

// sessionSortMode represents the sort mode for the session picker.
type sessionSortMode int

type pickerPurpose int

const (
	pickerPurposeModel pickerPurpose = iota
	pickerPurposeThinking
	pickerPurposeCommand
	pickerPurposeSettings
	pickerPurposeProviderSetup // Tab-accessible provider setup (login/endpoint)
)

type pickerItem struct {
	Label       string
	Value       string
	Detail      string
	Group       string
	Provider    string // Provider ID for unified model picker (e.g. "anthropic", "openai")
	NeedsSetup  bool   // True if provider needs auth/endpoint setup
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
	preset   Preset
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
	kind         SetupPromptKind
	provider     string
	providerName string
	value        string
	preset       Preset
	cfg          config.Config
	err          string
	saving       bool
	request      uint64
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
	ActivePreset      Preset
	PrintedTranscript bool
}

// ModelState holds the core backend, session, and storage handles.
type ModelState struct {
	Backend              Backend
	Session              session.Session
	Storage              session.Session
	Store                session.Store
	Switcher             Switcher
	Config               *config.Config
	Runtime              Snapshot
	Checkpoints          *ionworkspace.CheckpointStore
	EventGeneration      uint64
	RuntimeSwitchRequest uint64
	SettingsRequest      uint64
	// originalPrimaryModel stores the primary model name before cycling.
	// Used by buildAvailableModels to always have the full list.
	originalPrimaryModel string
	// Runner is the agent runner (Harness). When set, the TUI uses it
	// instead of Backend + Session for turn execution and events.
	Runner               runtime.Runner
}

// SubagentProgress, InFlightState, ProgressState are aliases for core types.

// PickerState holds state for the various overlay pickers.
type PickerState struct {
	Overlay            *pickerOverlayState
	Session            *sessionPickerState
	Setup              *setupPromptState
	ModelLoadRequest   uint64
	SessionLoadRequest uint64
	SetupSaveRequest   uint64
	OverlayClosedAt    time.Time
	PreStartupMode     bool
	SelectedSessionID  string
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

	// ToolOutputExpanded tracks whether tool output is globally expanded.
	// Toggled by Ctrl+O.
	ToolOutputExpanded bool

	// ThinkingBlockExpanded tracks whether thinking blocks are visible.
	// Toggled by Ctrl+T (Pi parity: app.thinking.toggle).
	ThinkingBlockExpanded bool

	// Keybindings manages keybinding configuration.
	Keybindings *KeybindingsManager

	// Styles (initialized once in New)
	st styles

	// GitWatcher monitors .git/HEAD for real-time branch change detection.
	GitWatcher *gitwatch.Watcher
}

func New(
	b Backend,
	s session.Session,
	store session.Store,
	workdir, branch, version string,
	switcher Switcher,
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

	var boot Bootstrap
	var sess session.Session
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
			ActivePreset: runtime.PresetPrimary,
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
		Keybindings:  NewKeybindingsManager(),
		st:           st,
		GitWatcher:   gitwatch.New(workdir),
	}

	if state, err := config.LoadState(); err == nil && state.ActivePreset != nil {
		m.App.ActivePreset = PresetFromString(*state.ActivePreset)
	}

	if cfg, err := config.Load(); err == nil {
		m.Model.Config = cfg
		m.Model.originalPrimaryModel = cfg.Model
		m.progressReducer().setReasoningEffort(normalizeThinkingValue(cfg.ReasoningEffort))
	} else {
		m.progressReducer().setReasoningEffort(config.DefaultReasoningEffort)
	}

	if s != nil {
		if usage, err := s.Usage(context.Background()); err == nil {
			m.progressReducer().applySessionUsage(usage.Input, usage.Output, usage.Cost.Total)
		}
	}
	m.loadInputHistory(context.Background())

	return m
}

// pasteImageFromClipboard reads an image from the clipboard and inserts its path.
func (m Model) pasteImageFromClipboard() (Model, tea.Cmd) {
	img, err := ionclipboard.ReadClipboardImage()
	if err != nil || img == nil {
		return m, nil
	}
	// Insert the file path into the composer
	return m, m.insertComposerText(img.FilePath)
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
	// Start git branch watcher
	if m.GitWatcher != nil {
		m.GitWatcher.OnChange(func(branch string) {
			// This callback runs from the watcher goroutine
		})
		m.GitWatcher.Start()
		cmds = append(cmds, m.pollGitBranch())
	}
	return tea.Batch(cmds...)
}

// pollGitBranch returns a command that polls for git branch changes.
func (m Model) pollGitBranch() tea.Cmd {
	return func() tea.Msg {
		time.Sleep(2 * time.Second)
		if m.GitWatcher == nil {
			return nil
		}
		branch := m.GitWatcher.Branch()
		return gitBranchChangedMsg{branch: branch}
	}
}

func (m Model) SelectedSessionID() string {
	return m.Picker.SelectedSessionID
}

func (m *Model) turnReducer() TurnReducer {
	return NewTurnReducer(&m.InFlight, &m.Progress)
}

func (m Model) WithPrintedTranscript(v bool) Model {
	m.App.PrintedTranscript = v
	return m
}

func (m Model) WithSize(width, height int) Model {
	m.App.Width = width
	m.App.Height = height
	m.layout()
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
	snapshot := newRuntimeSnapshot(
		cfg,
		runtimeCfg,
		PresetFromString(preset),
		"",
	).WithHandles(m.Handles())
	m.applyRuntimeSnapshot(snapshot)
	return m
}

func (m Model) WithActivePreset(value string) Model {
	m.App.ActivePreset = PresetFromString(value)
	return m
}

func (m Model) WithSessionPicker() Model {
	m, _ = m.openSessionPicker()
	return m
}

func (m Model) WithSessionPreStartupMode() Model {
	m.Picker.PreStartupMode = true
	m, _ = m.openSessionPicker()
	return m
}

func (m Model) WithProviderPicker() Model {
	m, _ = m.openProviderSetupPicker()
	return m
}

func (m Model) WithModelPicker() Model {
	m, _ = m.openModelPicker()
	return m
}

func (m Model) WithCheckpointStore(store *ionworkspace.CheckpointStore) Model {
	m.Model.Checkpoints = store
	return m
}

// WithRunner sets the agent runner (Harness) for the model.
// When set, the TUI uses the Runner for turn execution and events
// instead of Backend + Session directly.
func (m Model) WithRunner(r runtime.Runner) Model {
	m.Model.Runner = r
	if r != nil {
		m.Model.Session = r.Session()
	}
	return m
}
