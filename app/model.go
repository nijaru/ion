package app

import (
	"context"
	"errors"
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
	"github.com/nijaru/ion/session"
)

const (
	minComposerHeight = 1
	maxComposerHeight = 10
)

type sessionEventMsg struct {
	generation uint64
	reader     uint64
	cursor     agent.EventCursor
	event      session.Event
}

type runtimeSubscriptionMsg struct {
	generation   uint64
	subscription *agent.EventSubscription
	err          error
}

// eventSubscriptionState prevents more than one asynchronous subscription
// request from being in flight for a runtime generation. Init and a fast
// submit can otherwise both call awaitSessionEvent before the first
// subscription result reaches the Bubble Tea update loop; duplicate readers
// race on one event stream and make cursor recovery discard events.
type eventSubscriptionState struct {
	generation uint64
	pending    bool
	reader     uint64
	readerBusy bool
}

type streamClosedMsg struct {
	generation uint64
	err        error
}

type clearPendingMsg struct {
	action pendingAction
}

type deferredEnterMsg struct{}

type approvalResolveMsg struct {
	err error
}

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
	leafID        string
	keybindings   *KeybindingsManager
	printLines    []string
	replayEntries []session.Entry
	notice        string
	showStatus    bool
}

type reloadConfigLoadedMsg struct {
	requestID   uint64
	keybindings *KeybindingsManager
	cfg         *config.Config
	err         error
}

type TransitionCommittedMsg struct {
	switchID   uint64
	transition Transition
	notice     session.Entry
	err        error
	retry      *setupPromptState
}

type runtimeSwitchErrorMsg struct {
	switchID uint64
	err      error
	retry    *setupPromptState
}

type resumeSessionSelectedMsg struct {
	switchID  uint64
	sessionID string
	cfg       *config.Config
}

type allModelsLoadedMsg struct {
	requestID uint64
	items     []pickerItem // All models from all providers, with Provider field set
	catalog   llm.ModelCatalogResult
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

type busyInputResultMsg struct {
	action string
	text   string
	images []session.ImageContent
	err    error
}

type turnSubmitResultMsg struct {
	text   string
	draft  string
	images []session.ImageContent
	err    error
	rearm  bool
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
	namedOnly bool            // Filter to named sessions only
	sortMode  sessionSortMode // Current session-list sort mode
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
	ManualModel bool   // Opens the native model-ID prompt instead of selecting a catalog item.
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
	title       string
	items       []pickerItem
	filtered    []pickerItem
	index       int
	query       string
	purpose     pickerPurpose
	preset      Preset
	cfg         *config.Config
	loading     bool
	err         string
	warning     string
	request     uint64
	setup       bool
	loadContext context.Context
	loadCancel  context.CancelFunc
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

type branchSummaryPromptState struct {
	targetID   string
	choice     int
	custom     bool
	value      string
	navigating bool
	err        string
}

type approvalPromptState struct {
	request   session.ApprovalRequest
	resolving bool
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

// ConfigLoader loads the effective process configuration for /reload. The
// command host may inject process-lifetime overrides (for example CLI flags)
// while app retains config.Load as the standalone default.
type ConfigLoader func() (*config.Config, error)

// ModelState holds setup metadata, the active harness, and its auxiliary adapter.
type ModelState struct {
	Info                   RuntimeInfo
	Storage                RuntimeStorage
	SessionCatalog         agent.SessionCatalog
	InputHistory           agent.InputHistory
	Jobs                   JobController
	Memory                 MemoryController
	Checkpoints            CheckpointController
	Switcher               Switcher
	ConfigLoader           ConfigLoader
	Catalog                ModelCatalog
	EndpointResolver       *llm.EndpointResolver
	Config                 *config.Config
	Runtime                Snapshot
	EventGeneration        uint64
	EventCursor            agent.EventCursor
	EventSubscription      *agent.EventSubscription
	EventSubscriptionState *eventSubscriptionState
	RuntimeSwitchRequest   uint64
	SettingsRequest        uint64
	MemoryRequest          uint64
	CheckpointRequest      uint64
	// originalPrimaryModel stores the primary model name before cycling.
	// Used by buildAvailableModels to always have the full list.
	originalPrimaryModel string
	// Runner is the agent runner (Controller). When set, the TUI uses it as the
	// single turn and event owner.
	Runner agent.Runtime
	// Recovery contains startup action evidence that requires explicit
	// verification before retry. The runtime remains the authority; this is a
	// read-only frontend snapshot for status and /actions.
	Recovery []session.ActionRecord
	// RecoveryRequest identifies an in-flight explicit action reconciliation.
	// It prevents duplicate commands while the controller records evidence.
	RecoveryRequest uint64
	// ActiveTools is the runtime-owned active tool projection. It is refreshed
	// from RuntimeSnapshot on subscription/resync; ToolSurface remains the
	// registry-level startup description.
	ActiveTools []string
	// LeafID is the current runtime-selected tree leaf from the authoritative
	// event snapshot. It is a render/query hint, never a mutation authority.
	LeafID string
}

// PickerState holds state for the various overlay pickers.
type PickerState struct {
	Overlay            *pickerOverlayState
	Session            *sessionPickerState
	Setup              *setupPromptState
	BranchSummary      *branchSummaryPromptState
	Approval           *approvalPromptState
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
	Images                []session.ImageContent
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
	b RuntimeInfo,
	storage RuntimeStorage,
	catalog agent.SessionCatalog,
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
	m := Model{
		App: AppState{
			Workdir:      workdir,
			Branch:       branch,
			Version:      version,
			ActivePreset: PresetPrimary,
		},
		Model: ModelState{
			Info:                   b,
			Storage:                storage,
			SessionCatalog:         catalog,
			Recovery:               append([]session.ActionRecord(nil), boot.Recovery...),
			Switcher:               switcher,
			EndpointResolver:       llm.NewEndpointResolver(llm.EndpointResolverOptions{}),
			EventSubscriptionState: &eventSubscriptionState{},
		},
		InFlight: InFlightState{},
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
	m.Model.Catalog = llm.NewModelCatalog(llm.ModelCatalogOptions{
		EndpointResolver: m.Model.EndpointResolver,
	})

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

	if storage != nil {
		if usage, err := storage.Usage(context.Background()); err == nil {
			m.progressReducer().applySessionUsage(usage.Input, usage.Output, usage.Cost.Total)
		}
	}
	if history, ok := catalog.(agent.InputHistory); ok {
		m.Model.InputHistory = history
		m.loadInputHistory(context.Background())
	}

	return m
}

// WithJobs installs the runtime-owned background-job projection used by the
// TUI. The manager itself remains outside app/ so provider switches do not
// replace or persist process state.
func (m Model) WithJobs(jobs JobController) Model {
	m.Model.Jobs = jobs
	return m
}

// WithConfigLoader installs the process-owned effective config loader used by
// /reload. It keeps CLI/runtime overrides in the host layer without making the
// TUI know how startup flags are represented.
func (m Model) WithConfigLoader(loader ConfigLoader) Model {
	m.Model.ConfigLoader = loader
	return m
}

// WithModelCatalog installs the host-owned provider/model discovery service.
func (m Model) WithModelCatalog(catalog ModelCatalog) Model {
	m.Model.Catalog = catalog
	return m
}

// WithEndpointResolver installs the host-owned local endpoint resolver used by
// provider setup and model pickers. It must be the same resolver used by the
// injected model catalog and runtime provider construction.
func (m Model) WithEndpointResolver(resolver *llm.EndpointResolver) Model {
	m.Model.EndpointResolver = resolver
	return m
}

// WithMemory installs the explicit workspace-memory host used by /memory.
// Memory remains outside the session tree and is never injected into prompts.
func (m Model) WithMemory(memory MemoryController) Model {
	m.Model.Memory = memory
	return m
}

// pasteImageFromClipboard attaches an image to the next prompt and inserts its
// temporary path as a visible reference in the composer.
func (m Model) pasteImageFromClipboard() (Model, tea.Cmd) {
	img, err := ionclipboard.ReadClipboardImage()
	if err != nil || img == nil {
		return m, nil
	}
	return m.attachClipboardImage(img)
}

func (m Model) attachClipboardImage(img *ionclipboard.ImageData) (Model, tea.Cmd) {
	if img == nil || len(img.Bytes) == 0 {
		return m, nil
	}
	mimeType := strings.TrimSpace(img.MimeType)
	if mimeType == "" {
		mimeType = "image/png"
	}
	m.Input.Images = append(m.Input.Images, session.ImageContent{
		Data:     append([]byte(nil), img.Bytes...),
		MimeType: mimeType,
	})
	if img.FilePath == "" {
		return m, nil
	}
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

// WithCheckpoints installs the workspace recovery projection used by
// /rewind. The controller owns checkpoint storage and restore policy.
func (m Model) WithCheckpoints(checkpoints CheckpointController) Model {
	m.Model.Checkpoints = checkpoints
	return m
}

// WithRunner sets the agent runner (Controller) for the model. The TUI uses the
// Runner for turn execution and events rather than duplicating session state.
func (m Model) WithRunner(r agent.Runtime) Model {
	m.Model.Runner = r
	m.Model.SessionCatalog = nil
	m.Model.InputHistory = nil
	if catalog, ok := r.(agent.SessionCatalog); ok {
		m.Model.SessionCatalog = catalog
	}
	if history, ok := r.(agent.InputHistory); ok {
		m.Model.InputHistory = history
	}
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
		RuntimeRequired: m.Model.Info != nil,
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
		RuntimeRequired: m.Model.Info != nil,
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

func (m Model) runtimeHeaderLine(_ RuntimeInfo) string {
	version := strings.TrimSpace(m.App.Version)
	if version == "" {
		version = "v0.0.0"
	}
	return "ion " + version
}

var saveRuntimeState = config.SaveRuntimeState

func newRuntimeSnapshot(
	appCfg *config.Config,
	runtimeCfg *config.Config,
	preset Preset,
	status string,
) Snapshot {
	return NewSnapshot(appCfg, runtimeCfg, preset, status)
}

func newRuntimeTransition(
	appCfg *config.Config,
	runtimeCfg *config.Config,
	preset Preset,
	status string,
) Transition {
	return NewTransition(appCfg, runtimeCfg, preset, status)
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
	return m.beginRuntimeTransitionCommitWithRetry(t, notice, nil)
}

func (m Model) beginRuntimeTransitionCommitWithRetry(
	t Transition,
	notice session.Entry,
	retry *setupPromptState,
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
			return TransitionCommittedMsg{switchID: switchID, err: err, retry: retry}
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
		if msg.retry != nil {
			retry := *msg.retry
			retry.err = msg.err.Error()
			retry.saving = false
			retry.request = 0
			m.pickerReducer().openSetup(retry)
		}
		return m.handleLocalError(msg.err)
	}
	transition := msg.transition.WithHandles(m.Handles())
	if transition.PersistReasoning && m.Model.Runner != nil {
		if err := m.Model.Runner.SetThinking(context.Background(), thinkingLevelForRuntime(transition.Snapshot.Reasoning)); err != nil {
			previous := m.persistedReasoningEffort(transition.PersistReasoningSlot)
			if transition.PreviousReasoning != nil {
				previous = *transition.PreviousReasoning
			}
			rollback := config.RuntimeStateUpdate{
				ReasoningPreset:  transition.PersistReasoningSlot.String(),
				ReasoningEffort:  previous,
				PersistReasoning: true,
			}
			rollbackErr := saveRuntimeState(rollback)
			return m.handleLocalError(errors.Join(err, rollbackErr))
		}
	}
	m.applyRuntimeSnapshot(transition.Snapshot)
	m.clearProgressError()
	return m, m.terminalCommit().Entries(msg.notice)
}

func (m Model) persistedReasoningEffort(preset Preset) string {
	if m.Model.Config == nil {
		return config.DefaultReasoningEffort
	}
	if preset == PresetFast {
		return m.Model.Config.FastReasoningEffort
	}
	return m.Model.Config.ReasoningEffort
}

func providerSetupPrompt(ctx context.Context, cfg *config.Config, resolver *llm.EndpointResolver) (SetupPromptKind, error) {
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
		if err := ensureProviderReadyForSelection(ctx, cfg, resolver); err != nil {
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
	if state, err := config.LoadState(); err == nil {
		if preset == PresetFast {
			transition.PreviousReasoning = state.FastReasoningEffort
		} else {
			transition.PreviousReasoning = state.ReasoningEffort
		}
	}
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

func (m Model) Handles() Handles {
	return Handles{
		Info:    m.Model.Info,
		Runner:  m.Model.Runner,
		Storage: m.Model.Storage,
	}
}

func (m Model) runtimeProvider() string {
	if provider := strings.TrimSpace(m.Model.Runtime.Provider); provider != "" {
		return provider
	}
	if m.Model.Info == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Info.Provider())
}

func (m Model) runtimeModel() string {
	if model := strings.TrimSpace(m.Model.Runtime.Model); model != "" {
		return model
	}
	if m.Model.Info == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Info.Model())
}
