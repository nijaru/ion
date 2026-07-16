package app

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

// ProgressMode tracks the current turn lifecycle phase.
type ProgressMode int

const (
	StateReady ProgressMode = iota
	StateIonizing
	StateStreaming
	StateWorking
	StateComplete
	StateCancelled
	StateError
)

// TurnSummary records metrics from the most recent completed turn.
type TurnSummary struct {
	Elapsed time.Duration
	Input   int
	Output  int
	Cost    float64
}

// InFlightState holds data for the currently active turn or streaming response.
type InFlightState struct {
	Pending                 *session.Entry
	PendingTools            map[string]session.Entry
	CommittedAssistant      *session.Entry // assistant entry that survived tool clearing
	ReasonBuf               string
	StreamBuf               string
	StreamChunks            []string
	QueuedSteering          []string
	QueuedTurns             []string
	QueuedTurnsBackendOwned bool
	Thinking                bool
	Canceling               bool
	AgentCommitted          bool
	DrainUntilTurnStarted   bool
	DrainStartedAt          time.Time
}

// ProgressState holds turn-level metrics and overall progress status.
type ProgressState struct {
	Mode              ProgressMode
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
	LastTurnSummary   TurnSummary
	TokensSent        int
	TokensReceived    int
	ContextTokens     int
	TotalCost         float64
	LastToolUseID     string
}

// Backend is the agent backend interface.
type Backend = agent.Backend

// Preset represents a named model preset.
type Preset string

const (
	PresetPrimary Preset = "primary"
	PresetFast    Preset = "fast"
)

func PresetFromString(s string) Preset {
	switch s {
	case "fast":
		return PresetFast
	default:
		return PresetPrimary
	}
}

func (p Preset) String() string { return string(p) }

// Snapshot captures the current runtime state.
type Snapshot struct {
	AppConfig     config.Config
	BackendConfig config.Config
	Preset        Preset
	Status        string
	Reasoning     string
	Provider      string
	Model         string
	SessionID     string
	Materialized  bool
}

func (s Snapshot) WithHandles(h Handles) Snapshot { return s }

func NewSnapshot(appCfg, backendCfg *config.Config, preset Preset, status string) Snapshot {
	s := Snapshot{Preset: preset, Status: status}
	if appCfg != nil {
		s.AppConfig = *appCfg
	}
	if backendCfg != nil {
		s.BackendConfig = *backendCfg
	}
	return s
}

// Transition represents a pending config/state change.
type Transition struct {
	Snapshot             Snapshot
	PersistState         bool
	PersistReasoning     bool
	PersistActivePreset  bool
	PersistReasoningSlot Preset
}

func NewTransition(appCfg, backendCfg *config.Config, preset Preset, status string) Transition {
	if appCfg == nil {
		appCfg = &config.Config{}
	}
	if backendCfg == nil {
		backendCfg = appCfg
	}
	return Transition{
		Snapshot: Snapshot{
			AppConfig:     *appCfg,
			BackendConfig: *backendCfg,
			Preset:        preset,
			Status:        status,
			Provider:      backendCfg.Provider,
			Model:         backendCfg.Model,
		},
	}
}

func (t Transition) NeedsPersistence() bool { return t.PersistState || t.PersistActivePreset }
func (t Transition) WithHandles(h Handles) Transition {
	t.Snapshot.SessionID = ""
	t.Snapshot.Materialized = false
	if id, ok := GetSessionState(h); ok {
		t.Snapshot.SessionID = id
		t.Snapshot.Materialized = true
	}
	return t
}
func (t Transition) WithActivePresetPersistence(p ...Preset) Transition {
	t.PersistActivePreset = true
	t.PersistReasoningSlot = t.Snapshot.Preset
	return t
}
func (t Transition) Persist(fn func(update config.RuntimeStateUpdate) error) error {
	if fn == nil {
		return nil
	}
	return fn(config.RuntimeStateUpdate{})
}
func (t Transition) WithStatePersistence() Transition     { t.PersistState = true; return t }
func (t Transition) WithReasoningPersistence() Transition { t.PersistReasoning = true; return t }

// Accepted wraps a Transition with resolved Handles.
type Accepted struct {
	Transition Transition
	Handles    Handles
}

func NewAccepted(transition Transition, handles Handles) Accepted {
	return Accepted{Transition: transition, Handles: handles}
}

// SetupPromptKind enumerates first-run prompt types.
type SetupPromptKind int

const (
	SetupPromptAPIKey SetupPromptKind = iota + 1
	SetupPromptEndpoint
)

// Handles holds resolved runtime references.
type Handles struct {
	Backend Backend
	Runner  agent.Runner
	Storage persistenceAdapter
}

// Switcher creates a new backend, harness, and storage session for model switching.
type Switcher func(context.Context, *config.Config, string) (Backend, agent.Runner, session.Session, error)

// SwitchInput holds the parameters for a model switch.
type SwitchInput struct {
	Context         context.Context
	Config          *config.Config
	ProviderKey     string
	Switcher        Switcher
	Transition      Transition
	Current         Handles
	TargetSessionID string
	PreserveSession bool
	SaveState       func(update config.RuntimeStateUpdate) error
}

// SwitchResult holds the result of a model switch.
type SwitchResult struct {
	Runtime  Accepted
	Previous Handles
}

func (r SwitchResult) GetEntries(ctx context.Context, store session.Store) ([]session.Entry, error) {
	if store == nil {
		return nil, nil
	}
	return store.Entries(ctx)
}

// ResumeInput holds the parameters for resuming a session.
type ResumeInput struct {
	Switcher   Switcher
	Transition Transition
	Current    Handles
	SaveState  func(update config.RuntimeStateUpdate) error
	Context    context.Context
	Store      session.Store
	SessionID  string
}

// Switch constructs the replacement runtime but does not close input.Current.
// The UI applies the accepted result and closes the previous runner only after
// the replacement is ready; a failed switch therefore leaves the old runtime
// usable.
func Switch(ctx context.Context, input SwitchInput) (SwitchResult, error) {
	if input.Switcher == nil {
		return SwitchResult{}, fmt.Errorf("switcher not provided")
	}

	cfg := input.Config
	if cfg == nil {
		appCfg := input.Transition.Snapshot.AppConfig
		cfg = &appCfg
	}

	backend, runner, storage, err := input.Switcher(ctx, cfg, input.TargetSessionID)
	if err != nil {
		return SwitchResult{}, fmt.Errorf("switch: %w", err)
	}

	newHandles := Handles{
		Backend: backend,
		Runner:  runner,
		Storage: storage,
	}
	if err := validateRuntimeHandles(newHandles); err != nil {
		CloseHandles(newHandles)
		return SwitchResult{}, fmt.Errorf("switch: %w", err)
	}

	result := SwitchResult{
		Runtime: Accepted{
			Handles:    newHandles,
			Transition: input.Transition,
		},
		Previous: input.Current,
	}

	if input.SaveState != nil {
		if err := input.SaveState(config.RuntimeStateUpdate{
			Config: cfg,
		}); err != nil {
			CloseHandles(newHandles)
			return SwitchResult{}, fmt.Errorf("switch: persist runtime state: %w", err)
		}
	}

	return result, nil
}

// Resume constructs a runtime attached to an existing session. As with Switch,
// the caller owns the post-acceptance close of input.Current.
func Resume(ctx context.Context, input ResumeInput) (SwitchResult, error) {
	if input.Switcher == nil {
		return SwitchResult{}, fmt.Errorf("switcher not provided")
	}
	if input.SessionID == "" {
		return SwitchResult{}, fmt.Errorf("session id is required")
	}

	cfg := input.Transition.Snapshot.BackendConfig
	backend, runner, storage, err := input.Switcher(ctx, &cfg, input.SessionID)
	if err != nil {
		return SwitchResult{}, fmt.Errorf("resume: %w", err)
	}

	newHandles := Handles{Backend: backend, Runner: runner, Storage: storage}
	if err := validateRuntimeHandles(newHandles); err != nil {
		CloseHandles(newHandles)
		return SwitchResult{}, fmt.Errorf("resume: %w", err)
	}
	transition := input.Transition.WithHandles(newHandles)
	if input.SaveState != nil {
		if err := input.SaveState(config.RuntimeStateUpdate{Config: &cfg}); err != nil {
			CloseHandles(newHandles)
			return SwitchResult{}, fmt.Errorf("resume: persist runtime state: %w", err)
		}
	}
	return SwitchResult{
		Runtime:  NewAccepted(transition, newHandles),
		Previous: input.Current,
	}, nil
}

func validateRuntimeHandles(handles Handles) error {
	if handles.Backend == nil {
		return fmt.Errorf("incomplete runtime: backend is nil")
	}
	if handles.Runner == nil {
		return fmt.Errorf("incomplete runtime: runner is nil")
	}
	if handles.Storage == nil {
		return fmt.Errorf("incomplete runtime: session storage is nil")
	}
	return nil
}

// CloseHandles releases resources.
func CloseHandles(handles Handles) {
	if handles.Runner != nil {
		_ = handles.Runner.Close()
	}
}

// GetSessionState returns the active harness session ID if available.
func GetSessionState(h Handles) (string, bool) {
	if h.Runner == nil || h.Runner.Session() == nil {
		return "", false
	}
	return h.Runner.Session().ID(), true
}

// IsLocalBusyStatus returns true if the status indicates local activity.
func IsLocalBusyStatus(s string) bool {
	return s != "" && s != "idle" && s != "complete"
}

// IsCompactingStatus returns true if the status indicates compaction.
func IsCompactingStatus(s string) bool {
	return s == "compacting"
}

// ProviderSelection tracks provider routing.
type ProviderSelection struct {
	Setup                SetupPromptKind
	Config               *config.Config
	SupportsModelListing bool
	Transition           Transition
}

// --- TurnReducer ---

// TurnReducer manages the state machine for a single turn.
type TurnReducer struct {
	inFlight *InFlightState
	progress *ProgressState
}

func NewTurnReducer(inFlight *InFlightState, progress *ProgressState) TurnReducer {
	return TurnReducer{inFlight: inFlight, progress: progress}
}

func (t TurnReducer) AgentStreamContent() string {
	if t.inFlight == nil {
		return ""
	}
	return t.inFlight.StreamBuf
}

func (t TurnReducer) ClearActiveState(full bool) {
	if t.inFlight == nil {
		return
	}
	t.inFlight.Thinking = false
	t.inFlight.Pending = nil
	t.inFlight.PendingTools = nil
	t.inFlight.CommittedAssistant = nil
	t.inFlight.StreamBuf = ""
	t.inFlight.ReasonBuf = ""
	t.inFlight.StreamChunks = nil
	t.inFlight.AgentCommitted = false
	t.inFlight.DrainUntilTurnStarted = false
	t.inFlight.DrainStartedAt = time.Time{}
	t.inFlight.Canceling = false
	if t.progress != nil {
		t.progress.LastToolUseID = ""
		t.progress.ContextTokens = 0
	}
	if full {
		t.inFlight.QueuedTurns = nil
		t.inFlight.QueuedSteering = nil
		t.inFlight.QueuedTurnsBackendOwned = false
	}
}

func (t TurnReducer) ResetFinishedTurnSummary() {
	if t.progress != nil {
		t.progress.LastTurnSummary = TurnSummary{}
	}
}
func (t TurnReducer) PopQueuedTurn() string {
	if t.inFlight == nil || len(t.inFlight.QueuedTurns) == 0 {
		return ""
	}
	text := t.inFlight.QueuedTurns[0]
	t.inFlight.QueuedTurns = t.inFlight.QueuedTurns[1:]
	return text
}

func (t TurnReducer) StartSubmit() {
	if t.progress != nil {
		t.progress.Mode = StateIonizing
		t.progress.Status = "Submitting..."
	}
}
func (t TurnReducer) RejectSubmit(reason string) {}
func (t TurnReducer) SetBackendQueuedInput(steering []string, followUp []string) {
	if t.inFlight == nil {
		return
	}
	if !t.inFlight.QueuedTurnsBackendOwned && len(steering) == 0 && len(followUp) == 0 {
		// An empty backend snapshot must not erase a locally queued turn.
		return
	}
	t.inFlight.QueuedSteering = append([]string(nil), steering...)
	t.inFlight.QueuedTurns = append([]string(nil), followUp...)
	t.inFlight.QueuedTurnsBackendOwned = true
}

func (t TurnReducer) QueueTurn(text string) {
	if t.inFlight != nil {
		t.inFlight.QueuedTurns = append(t.inFlight.QueuedTurns, text)
	}
}

func (t TurnReducer) ClearQueuedTurns() {
	if t.inFlight != nil {
		t.inFlight.QueuedTurns = nil
	}
}

func (t TurnReducer) DrainingUntilTurnStarted() bool {
	return t.inFlight != nil && t.inFlight.DrainUntilTurnStarted
}

// CancelDecision holds the result of a cancel attempt.
type CancelDecision struct {
	EntryContent string
}

func (t TurnReducer) CancelTurn(reason string, now time.Time) CancelDecision {
	if t.inFlight != nil {
		t.inFlight.Canceling = true
		t.inFlight.QueuedTurns = nil
	}
	if t.progress != nil {
		t.progress.Mode = StateCancelled
	}
	return CancelDecision{EntryContent: reason}
}

func (t TurnReducer) FinishDrain() {
	if t.inFlight != nil {
		t.inFlight.DrainUntilTurnStarted = false
		t.inFlight.DrainStartedAt = time.Time{}
	}
}
func (t TurnReducer) StreamClosed(now time.Time) (session.Entry, bool) {
	if t.inFlight == nil || t.inFlight.Pending == nil {
		return nil, false
	}
	entry := *t.inFlight.Pending
	t.inFlight.Pending = nil
	t.inFlight.StreamBuf = ""
	t.inFlight.ReasonBuf = ""
	t.inFlight.StreamChunks = nil
	return entry, true
}
func (t TurnReducer) FailTurn(msg string, now time.Time) {}
func (t TurnReducer) ClearLocalErrorIfIdle()             {}

// StatusChangedDecision holds the result of a status change.
type StatusChangedDecision struct {
	Root             bool
	PersistTimestamp time.Time
	Status           string
}

func (t TurnReducer) ApplyStatusChangedInput(msg interface{}) StatusChangedDecision {
	return StatusChangedDecision{}
}

func providerRetryStatus(msg session.ProviderRetry) string {
	reason := "transient provider failure"
	if msg.Err != nil {
		reason = strings.Join(strings.Fields(msg.Err.Error()), " ")
		if len(reason) > 160 {
			reason = reason[:160] + "..."
		}
	}
	if msg.Delay <= 0 {
		return fmt.Sprintf("Provider error: %s. Retrying now... Ctrl+C stops.", reason)
	}
	return fmt.Sprintf("Provider error: %s. Retrying in %s... Ctrl+C stops.", reason, msg.Delay)
}

func (t TurnReducer) StartTurn(now time.Time, ts time.Time) {
	if t.inFlight != nil {
		t.inFlight.Thinking = true
		t.inFlight.Canceling = false
	}
	if t.progress != nil {
		t.progress.Mode = StateStreaming
		t.progress.Status = "Streaming..."
		t.progress.TurnStartedAt = now
		t.progress.CurrentTurnInput = 0
		t.progress.CurrentTurnOutput = 0
		t.progress.CurrentTurnCost = 0
		t.progress.BudgetStopReason = ""
		t.progress.StatusUpdatedAt = ts
	}
}
func (t TurnReducer) StopThinking() {
	if t.inFlight != nil {
		t.inFlight.Thinking = false
	}
}

func (t TurnReducer) FinishPendingAssistant() (session.Entry, bool, bool) {
	if t.inFlight == nil {
		return nil, false, false
	}
	completed := t.inFlight.AgentCommitted
	// Pending may have been cleared by CompleteToolResult. Use the committed
	// assistant entry if available, falling back to Pending.
	var entry session.Entry
	if t.inFlight.Pending != nil {
		entry = *t.inFlight.Pending
	} else if t.inFlight.CommittedAssistant != nil {
		entry = *t.inFlight.CommittedAssistant
	}
	if entry == nil {
		return nil, completed, false
	}
	// Update the entry content from stream buffer if available.
	switch e := entry.(type) {
	case *session.MessageEntry:
		if am, ok := e.Message.(*session.AssistantMessage); ok {
			if t.inFlight.StreamBuf != "" {
				am.Content = []session.Content{session.TextContent{Text: t.inFlight.StreamBuf}}
			}
			if t.inFlight.ReasonBuf != "" {
				am.Content = append([]session.Content{session.ThinkingContent{Text: t.inFlight.ReasonBuf}}, am.Content...)
			}
		}

	}
	t.inFlight.Pending = nil
	t.inFlight.CommittedAssistant = nil
	t.inFlight.StreamBuf = ""
	t.inFlight.ReasonBuf = ""
	t.inFlight.StreamChunks = nil
	return entry, completed, true
}

func (t TurnReducer) RecordFinishedTurnSummary(now time.Time) {}

func (t TurnReducer) FinishTurnMode(completed bool) (session.Entry, bool) {
	if t.inFlight == nil {
		return nil, false
	}
	if !completed {
		errMsg := "turn finished without assistant response"
		if t.progress != nil {
			t.progress.Mode = StateError
			t.progress.LastError = errMsg
			t.progress.Status = ""
		}
		t.ClearActiveState(true)
		now := time.Now()
		return &session.MessageEntry{
			EntryBase: session.EntryBase{Timestamp: now},
			Message: &session.UserMessage{
				Content:   []session.Content{session.TextContent{Text: "Error: " + errMsg}},
				Timestamp: now,
			},
		}, true
	}
	if t.progress != nil {
		t.progress.Mode = StateIonizing
	}
	return nil, true
}

// TurnFinishedDispatch holds events to emit after a turn finishes.
type TurnFinishedDispatch struct {
	ReloadGitDiff      bool
	AwaitNext          bool
	Action             string
	Text               string
	RearmSessionEvents bool
}

var TurnFinishedDispatchSubmitLocal = "submit_local"

func (t TurnReducer) FinishTurnDispatch() TurnFinishedDispatch {
	if t.inFlight == nil {
		return TurnFinishedDispatch{AwaitNext: true}
	}
	if !t.inFlight.QueuedTurnsBackendOwned {
		if text := t.PopQueuedTurn(); text != "" {
			return TurnFinishedDispatch{
				Action:             TurnFinishedDispatchSubmitLocal,
				Text:               text,
				RearmSessionEvents: true,
			}
		}
	}
	return TurnFinishedDispatch{AwaitNext: true}
}

func (t TurnReducer) ApplyTokenUsage(msg interface{}) {
	if t.progress == nil {
		return
	}
	if m, ok := msg.(session.Message); ok {
		in, out, cost := session.TokenUsage(m)
		t.progress.CurrentTurnInput += in
		t.progress.CurrentTurnOutput += out
		t.progress.CurrentTurnCost += cost
		t.progress.TokensSent += in
		t.progress.TokensReceived += out
		t.progress.TotalCost += cost
	}
}

func (t TurnReducer) AppendAgentDelta(agentID string, delta interface{}, ts time.Time) {
	if t.inFlight == nil {
		return
	}
	var text string
	switch d := delta.(type) {
	case session.TextDelta:
		text = d.Text
	case *session.TextDelta:
		if d != nil {
			text = d.Text
		}
	}
	if text == "" {
		return
	}
	t.inFlight.StreamChunks = append(t.inFlight.StreamChunks, text)
	t.inFlight.StreamBuf += text
	t.syncFallbackAssistant(ts)
}

func (t TurnReducer) AppendThinkingDelta(agentID string, delta interface{}) {
	if t.inFlight == nil {
		return
	}
	switch d := delta.(type) {
	case session.ThinkingDelta:
		t.inFlight.ReasonBuf += d.Text
	case *session.ThinkingDelta:
		if d != nil {
			t.inFlight.ReasonBuf += d.Text
		}
	}
	if t.inFlight.ReasonBuf != "" {
		t.syncFallbackAssistant(time.Now())
	}
}

func (t TurnReducer) syncFallbackAssistant(ts time.Time) {
	if t.inFlight == nil {
		return
	}
	var assistant *session.AssistantMessage
	if t.inFlight.Pending != nil {
		if entry, ok := (*t.inFlight.Pending).(*session.MessageEntry); ok {
			assistant, _ = entry.Message.(*session.AssistantMessage)
		}
	}
	if assistant == nil {
		assistant = &session.AssistantMessage{Timestamp: ts}
		entry := &session.MessageEntry{
			EntryBase: session.EntryBase{Timestamp: ts},
			Message:   assistant,
		}
		var e session.Entry = entry
		t.inFlight.Pending = &e
	}
	content := make([]session.Content, 0, 2)
	if t.inFlight.ReasonBuf != "" {
		content = append(content, session.ThinkingContent{Text: t.inFlight.ReasonBuf})
	}
	if t.inFlight.StreamBuf != "" {
		content = append(content, session.TextContent{Text: t.inFlight.StreamBuf})
	}
	assistant.Content = content
}

func (t TurnReducer) StartAssistantMessage(msg session.Message) {
	am, ok := msg.(*session.AssistantMessage)
	if !ok || t.inFlight == nil {
		return
	}
	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{Timestamp: am.Timestamp},
		Message:   am,
	}
	var e session.Entry = entry
	t.inFlight.Pending = &e
	t.inFlight.StreamBuf = session.MessageText(am)
	t.inFlight.ReasonBuf = assistantReasoning(am)
}

func (t TurnReducer) UpdateAssistantMessage(msg session.Message) {
	am, ok := msg.(*session.AssistantMessage)
	if !ok || t.inFlight == nil {
		return
	}
	if t.inFlight.Pending == nil || session.EntryRole(*t.inFlight.Pending) != session.RoleAgent {
		t.StartAssistantMessage(am)
		return
	}
	entry, ok := (*t.inFlight.Pending).(*session.MessageEntry)
	if !ok {
		return
	}
	entry.Message = am
	t.inFlight.StreamBuf = session.MessageText(am)
	t.inFlight.ReasonBuf = assistantReasoning(am)
}

func assistantReasoning(msg *session.AssistantMessage) string {
	var b strings.Builder
	for _, content := range msg.Content {
		if thinking, ok := content.(session.ThinkingContent); ok {
			b.WriteString(thinking.Text)
		}
	}
	return b.String()
}

func (t TurnReducer) CommitAgentMessage(msg interface{}) (session.Entry, bool) {
	if t.inFlight == nil {
		return nil, false
	}
	switch m := msg.(type) {
	case *session.AssistantMessage:
		entry := &session.MessageEntry{
			EntryBase: session.EntryBase{Timestamp: m.Timestamp},
			Message:   m,
		}
		var e session.Entry = entry
		t.inFlight.Pending = &e
		t.inFlight.CommittedAssistant = &e
		t.inFlight.AgentCommitted = true
		return entry, true
	}
	return nil, false
}

func (t TurnReducer) StartToolCall(id string, ts time.Time, title string) {
	if t.inFlight == nil {
		return
	}
	if t.inFlight.PendingTools == nil {
		t.inFlight.PendingTools = make(map[string]session.Entry)
	}
	t.inFlight.PendingTools[id] = &session.MessageEntry{
		EntryBase: session.EntryBase{Timestamp: ts},
		Message: &session.ToolResultMessage{
			ToolName:  title,
			Timestamp: ts,
		},
	}
	if t.progress != nil {
		t.progress.LastToolUseID = id
		t.progress.Mode = StateWorking
		t.progress.Status = "Working..."
	}
}

func (t TurnReducer) AppendToolOutput(id string, output string, isError bool) {
	if t.inFlight == nil || t.inFlight.PendingTools == nil {
		return
	}
	entry, ok := t.inFlight.PendingTools[id]
	if !ok {
		return
	}
	if me, ok := entry.(*session.MessageEntry); ok {
		if tr, ok := me.Message.(*session.ToolResultMessage); ok {
			tr.Content = append(tr.Content, session.TextContent{Text: output})
			if isError {
				tr.IsError = true
			}
		}
	}
}

func (t TurnReducer) CompleteToolResult(id string, msg interface{}) (session.Entry, bool) {
	if t.inFlight == nil {
		return nil, false
	}
	if t.inFlight.PendingTools != nil {
		delete(t.inFlight.PendingTools, id)
	}
	if len(t.inFlight.PendingTools) > 0 {
		for _, entry := range t.inFlight.PendingTools {
			t.inFlight.Pending = &entry
			break
		}
	} else {
		t.inFlight.Pending = nil
		if t.progress != nil {
			t.progress.Mode = StateIonizing
			t.progress.Status = ""
			t.progress.ContextTokens = 0
		}
	}
	var entry session.Entry
	switch m := msg.(type) {
	case session.ToolExecEnd:
		entry = &session.MessageEntry{
			EntryBase: session.EntryBase{Timestamp: time.Now()},
			Message:   &m.Result,
		}
	default:
		now := time.Now()
		entry = &session.MessageEntry{
			EntryBase: session.EntryBase{Timestamp: now},
			Message: &session.ToolResultMessage{
				ToolCallID: id,
				Timestamp:  now,
			},
		}
	}
	return entry, true
}

// --- Agent type aliases (re-exported for dependent packages) ---

type Bootstrap = agent.Bootstrap
type Compactor = agent.Compactor
type ToolSurface = agent.ToolSurface
type ToolSummarizer = agent.ToolSummarizer
