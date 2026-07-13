package app

import (
	"context"
	"fmt"
	"strings"
	"time"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"

	ionclipboard "github.com/nijaru/ion/internal/clipboard"
	"github.com/nijaru/ion/internal/gitwatch"
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
	items     []sessionPickerItem
	filtered  []sessionPickerItem
	index     int
	query     string
	err       string
	loading   bool
	request   uint64
	namedOnly bool            // Pi parity: filter to named sessions only
	sortMode  sessionSortMode // Pi parity: sort mode for session list
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

// persistenceAdapter is the narrow TUI capability for display-only session writes
// and reads. The harness remains the owner of the active session and turn state.
type persistenceAdapter interface {
	ID() string
	Meta() session.Metadata
	Entries(context.Context) ([]session.Entry, error)
	Usage(context.Context) (session.Usage, error)
}

// ModelState holds setup metadata, the active harness, and its auxiliary adapter.
type ModelState struct {
	Backend              Backend
	Storage              persistenceAdapter
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
	Runner agent.Runner
}

// PickerState holds state for the various overlay pickers.
type PickerState struct {
	Overlay            *pickerOverlayState
	Session            *sessionPickerState
	Setup              *setupPromptState
	Tree               *treePickerState
	LastEscAt          time.Time
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
	if b != nil {
		boot = b.Bootstrap()
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
			ActivePreset: PresetPrimary,
		},
		Model: ModelState{
			Backend:     b,
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
	if m.Model.Runner != nil {
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
func (m Model) WithRunner(r agent.Runner) Model {
	m.Model.Runner = r
	return m
}
func (m Model) configurationStatus() string {
	decision := m.submitPreflightWithoutBudget()
	if decision.Allowed {
		return ""
	}
	return decision.Reason
}

func (m Model) submitPreflightWithoutBudget() SubmitPreflightDecision {
	return DecideSubmitPreflight(SubmitPreflightInput{
		RuntimeRequired: m.Model.Backend != nil,
		Provider:        m.runtimeProvider(),
		Model:           m.runtimeModel(),
	})
}

func (m Model) submitPreflight() SubmitPreflightDecision {
	var maxSessionCost float64
	if m.Model.Config != nil {
		maxSessionCost = m.Model.Config.MaxSessionCost
	}
	return DecideSubmitPreflight(SubmitPreflightInput{
		RuntimeRequired: m.Model.Backend != nil,
		Provider:        m.runtimeProvider(),
		Model:           m.runtimeModel(),
		TotalCost:       m.Progress.TotalCost,
		MaxSessionCost:  maxSessionCost,
	})
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

func (m Model) routingDecision(decision, reason, stopReason string) StoreRoutingDecision {
	provider := m.runtimeProvider()
	model := m.runtimeModel()
	var maxSessionCost, maxTurnCost float64
	if m.Model.Config != nil {
		maxSessionCost = m.Model.Config.MaxSessionCost
		maxTurnCost = m.Model.Config.MaxTurnCost
	}
	return StoreRoutingDecision{
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
		TS:             time.Now(),
	}
}

func (m Model) runtimeHeaderLine(_ Backend) string {
	version := strings.TrimSpace(m.App.Version)
	if version == "" {
		version = "v0.0.0"
	}
	return "ion " + version
}

var saveRuntimeState = config.SaveRuntimeState

func newRuntimeSnapshot(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset Preset,
	status string,
) Snapshot {
	return NewSnapshot(appCfg, backendCfg, preset, status)
}

func newRuntimeTransition(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset Preset,
	status string,
) Transition {
	return NewTransition(appCfg, backendCfg, preset, status)
}

func (m Model) commitRuntimeTransition(t Transition) (Model, error) {
	if t.NeedsPersistence() {
		return m, fmt.Errorf("runtime transition requires asynchronous persistence")
	}
	t = t.WithHandles(m.Handles())
	m.applyRuntimeSnapshot(t.Snapshot)
	return m, nil
}

func (m Model) beginRuntimeTransitionCommit(
	t Transition,
	notice session.Entry,
) (Model, tea.Cmd) {
	if !t.NeedsPersistence() {
		var err error
		m, err = m.commitRuntimeTransition(t)
		if err != nil {
			return m, TransitionErrorCmd(err)
		}
		return m, m.terminalCommit().Entries(notice)
	}
	switchID := m.runtimeRequest().begin("Saving runtime settings...")
	return m, func() tea.Msg {
		if err := t.Persist(saveRuntimeState); err != nil {
			return TransitionCommittedMsg{switchID: switchID, err: err}
		}
		return TransitionCommittedMsg{
			switchID:   switchID,
			transition: t,
			notice:     notice,
		}
	}
}

func (m Model) handleRuntimeTransitionCommitted(
	msg TransitionCommittedMsg,
) (Model, tea.Cmd) {
	if !m.runtimeRequest().finish(msg.switchID) {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	transition := msg.transition.WithHandles(m.Handles())
	m.applyRuntimeSnapshot(transition.Snapshot)
	m.clearProgressError()
	return m, m.terminalCommit().Entries(msg.notice)
}

func (m Model) ProviderSelection(
	ctx context.Context,
	cfg *config.Config,
	provider string,
	preset Preset,
) (ProviderSelection, error) {
	updated, err := updateProviderSelection(cfg, provider)
	if err != nil {
		return ProviderSelection{}, err
	}
	return providerSelectionForConfig(ctx, updated, preset)
}

func providerSelectionForConfig(
	ctx context.Context,
	updated *config.Config,
	preset Preset,
) (ProviderSelection, error) {
	setup, err := providerSetupPrompt(ctx, updated)
	if err != nil {
		return ProviderSelection{Config: updated}, err
	}
	if setup != 0 {
		return ProviderSelection{Config: updated, Setup: setup}, nil
	}
	selection := ProviderSelection{
		Config:               updated,
		SupportsModelListing: llm.SupportsModelListing(updated),
	}
	if !selection.SupportsModelListing {
		selection.Transition = newRuntimeTransition(
			updated,
			updated,
			preset,
			noModelConfiguredStatus(),
		).WithStatePersistence().WithActivePresetPersistence()
	}
	return selection, nil
}

func providerSetupPrompt(ctx context.Context, cfg *config.Config) (SetupPromptKind, error) {
	if cfg == nil || strings.TrimSpace(cfg.Provider) == "" {
		return 0, nil
	}
	def, ok := llm.Lookup(cfg.Provider)
	if !ok {
		return 0, fmt.Errorf("unsupported provider %q", strings.TrimSpace(cfg.Provider))
	}
	missingAuth := llm.RequiresAuth(cfg, def) &&
		llm.ResolvedAuthToken(cfg, def) == ""
	if def.ID == llm.OpenAICompatibleID {
		if missingAuth && strings.TrimSpace(cfg.Endpoint) != "" {
			return SetupPromptAPIKey, nil
		}
		if err := ensureProviderReadyForSelection(ctx, cfg); err != nil {
			return SetupPromptEndpoint, nil
		}
		if missingAuth {
			return SetupPromptAPIKey, nil
		}
		return 0, nil
	}
	if missingAuth {
		return SetupPromptAPIKey, nil
	}
	return 0, nil
}

func (m Model) modelSelectionTransition(
	cfg *config.Config,
	preset Preset,
	model string,
) (Transition, *config.Config, error) {
	updated := updateModelForPreset(cfg, model, preset)
	runtimeCfg, err := m.runtimeConfigForPreset(updated, preset)
	if err != nil {
		return Transition{}, nil, err
	}
	transition := newRuntimeTransition(updated, runtimeCfg, preset, "").
		WithStatePersistence()
	return transition, runtimeCfg, nil
}

func (m Model) thinkingSelectionTransition(
	cfg *config.Config,
	preset Preset,
	level string,
) (Transition, *config.Config, error) {
	updated := updateThinkingForPreset(cfg, level, preset)
	runtimeCfg, err := m.runtimeConfigForPreset(updated, preset)
	if err != nil {
		return Transition{}, nil, err
	}
	transition := newRuntimeTransition(updated, runtimeCfg, preset, "").
		WithReasoningPersistence()
	return transition, runtimeCfg, nil
}

func resumeSelectionTransition(cfg *config.Config) Transition {
	return newRuntimeTransition(
		cfg,
		cfg,
		PresetPrimary,
		"",
	).WithActivePresetPersistence()
}

func TransitionErrorCmd(err error) tea.Cmd {
	if err == nil {
		return nil
	}
	return func() tea.Msg {
		return localErrorMsg{err: err}
	}
}

func (m *Model) applyRuntimeSnapshot(snapshot Snapshot) {
	appCfg := snapshot.AppConfig
	backendCfg := snapshot.BackendConfig

	if m.Model.Backend != nil {
		m.Model.Backend.SetConfig(&backendCfg)
	}
	m.Model.Config = &appCfg
	m.Model.Runtime = snapshot
	m.App.ActivePreset = snapshot.Preset
	m.progressReducer().applyRuntimeSnapshot(snapshot)
}

func (m *Model) refreshRuntimeSessionSnapshot() {
	sessionID, materialized := GetSessionState(m.Handles())
	m.Model.Runtime.SessionID = sessionID
	m.Model.Runtime.Materialized = materialized
}

func (m Model) activeSession() session.Session {
	if m.Model.Runner == nil {
		return nil
	}
	return m.Model.Runner.Session()
}

func (m Model) Handles() Handles {
	return Handles{
		Backend: m.Model.Backend,
		Runner:  m.Model.Runner,
		Storage: m.Model.Storage,
	}
}

func (m Model) runtimeProvider() string {
	if provider := strings.TrimSpace(m.Model.Runtime.Provider); provider != "" {
		return provider
	}
	if m.Model.Backend == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Backend.Provider())
}

func (m Model) runtimeModel() string {
	if model := strings.TrimSpace(m.Model.Runtime.Model); model != "" {
		return model
	}
	if m.Model.Backend == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Backend.Model())
}
