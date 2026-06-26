package runtime

import (
	"context"
	"fmt"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

// ProgressMode tracks the current turn lifecycle phase.
type ProgressMode int

const (
	StateReady      ProgressMode = iota
	StateIonizing
	StateStreaming
	StateWorking
	StateComplete
	StateCancelled
	StateBlocked
	StateError
)

// TurnSummary records metrics from the most recent completed turn.
type TurnSummary struct {
	Elapsed time.Duration
	Input   int
	Output  int
	Cost    float64
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
	Pending                 *session.Entry
	PendingTools            map[string]session.Entry
	Subagents               map[string]*SubagentProgress
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
	return Snapshot{Preset: preset, Status: status}
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
	if h.Session != nil {
		t.Snapshot.SessionID = h.Session.ID()
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
func (t Transition) WithStatePersistence() Transition  { t.PersistState = true; return t }
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
	Session session.Session
	Storage session.Session
}

// Switcher creates a new backend and sessions for model switching.
type Switcher func(context.Context, *config.Config, string) (Backend, session.Session, session.Session, error)

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

// Switch performs a model switch.
func Switch(ctx context.Context, input SwitchInput) (SwitchResult, error) {
	if input.Switcher == nil {
		return SwitchResult{}, fmt.Errorf("switcher not provided")
	}

	cfg := input.Config
	if cfg == nil {
		appCfg := input.Transition.Snapshot.AppConfig
		cfg = &appCfg
	}

	backend, sess, storage, err := input.Switcher(ctx, cfg, input.TargetSessionID)
	if err != nil {
		return SwitchResult{}, fmt.Errorf("switch: %w", err)
	}

	newHandles := Handles{
		Backend: backend,
		Session: sess,
		Storage: storage,
	}

	result := SwitchResult{
		Runtime: Accepted{
			Handles:    newHandles,
			Transition: input.Transition,
		},
		Previous: input.Current,
	}

	if input.SaveState != nil {
		_ = input.SaveState(config.RuntimeStateUpdate{
			Config: cfg,
		})
	}

	return result, nil
}

// Resume re-attaches to an existing session.
func Resume(ctx context.Context, input ResumeInput) (SwitchResult, error) {
	return SwitchResult{}, fmt.Errorf("resume not implemented")
}

// CloseHandles releases resources.
func CloseHandles(handles Handles) {}

// GetSessionState returns the session ID if available.
func GetSessionState(h Handles) (string, bool) {
	if h.Session == nil {
		return "", false
	}
	return h.Session.ID(), true
}

// SessionStateInfo summarizes session state.
type SessionStateInfo struct {
	Backend Backend
	Session session.Session
	Store   session.Store
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

func (t TurnReducer) AgentStreamContent() string { return "" }

func (t TurnReducer) ClearActiveState(full bool) {
	if t.inFlight == nil {
		return
	}
	t.inFlight.Thinking = false
	t.inFlight.Pending = nil
	t.inFlight.PendingTools = nil
	t.inFlight.Subagents = nil
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

func (t TurnReducer) ResetFinishedTurnSummary() {}
func (t TurnReducer) setReasoningEffort(v int)  {}
func (t TurnReducer) applySessionUsage(in, out int, cost float64) {}

func (t TurnReducer) PopQueuedTurn() string {
	if t.inFlight == nil || len(t.inFlight.QueuedTurns) == 0 {
		return ""
	}
	text := t.inFlight.QueuedTurns[0]
	t.inFlight.QueuedTurns = t.inFlight.QueuedTurns[1:]
	return text
}

func (t TurnReducer) StartSubmit()            {}
func (t TurnReducer) RejectSubmit(reason string) {}
func (t TurnReducer) SetBackendQueuedInput(steering []string, followUp []string) {}

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

func (t TurnReducer) CancelActiveTurn(reason string, now time.Time) error { return nil }
func (t TurnReducer) DrainingUntilTurnStarted() bool { return false }

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

func (t TurnReducer) FinishDrain() {}
func (t TurnReducer) StreamClosed(now time.Time) (session.Entry, bool) { return nil, false }
func (t TurnReducer) FailTurn(msg string, now time.Time)              {}
func (t TurnReducer) ClearLocalErrorIfIdle()                           {}

// StatusChangedDecision holds the result of a status change.
type StatusChangedDecision struct {
	Root             bool
	PersistTimestamp time.Time
	Status           string
}

func (t TurnReducer) ApplyStatusChangedInput(msg interface{}) StatusChangedDecision {
	return StatusChangedDecision{}
}

func (t TurnReducer) StartTurn(now time.Time, ts time.Time) {}
func (t TurnReducer) StopThinking() {
	if t.inFlight != nil {
		t.inFlight.Thinking = false
	}
}

func (t TurnReducer) FinishPendingAssistant() (session.Entry, bool, bool) {
	if t.inFlight == nil || t.inFlight.Pending == nil {
		return nil, false, false
	}
	entry := *t.inFlight.Pending
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
	case *session.TestEntry:
		if t.inFlight.StreamBuf != "" {
			e.Content = t.inFlight.StreamBuf
		}
		if t.inFlight.ReasonBuf != "" {
			e.Reasoning = t.inFlight.ReasonBuf
		}
	}
	completed := t.inFlight.AgentCommitted
	t.inFlight.Pending = nil
	t.inFlight.StreamBuf = ""
	t.inFlight.ReasonBuf = ""
	t.inFlight.StreamChunks = nil
	return entry, completed, true
}

func (t TurnReducer) RecordFinishedTurnSummary(now time.Time) {}
func (t TurnReducer) BeginDrain(now time.Time)                {}

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
	ReloadGitDiff    bool
	AwaitNext        bool
	Action           string
	Text             string
	RearmSessionEvents bool
}

var TurnFinishedDispatchSubmitLocal = "submit_local"

func (t TurnReducer) FinishTurnDispatch() TurnFinishedDispatch {
	return TurnFinishedDispatch{}
}

func (t TurnReducer) ApplyTokenUsage(msg interface{}) {}

func (t TurnReducer) AppendAgentDelta(agentID string, delta interface{}, ts time.Time) {
	if t.inFlight == nil {
		return
	}
	t.inFlight.StreamChunks = append(t.inFlight.StreamChunks, fmt.Sprint(delta))
	t.inFlight.StreamBuf += fmt.Sprint(delta)
}

func (t TurnReducer) AppendThinkingDelta(agentID string, delta interface{}) {
	if t.inFlight == nil {
		return
	}
	t.inFlight.ReasonBuf += fmt.Sprint(delta)
}

func (t TurnReducer) ApplyBudgetStop(reason string, ts time.Time) (session.Entry, error) {
	return nil, nil
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
	t.inFlight.PendingTools[id] = &session.TestEntry{
		Role:      session.RoleTool,
		Title:     title,
		Timestamp: ts,
	}
	if t.progress != nil {
		t.progress.LastToolUseID = id
	}
}

func (t TurnReducer) AppendToolOutput(id string, output string, isError bool)  {}
func (t TurnReducer) AppendToolError(id, name, err string, ts time.Time)       {}

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

func (t TurnReducer) RequestChild(name, intent string) SubagentProgress {
	if t.inFlight == nil {
		return SubagentProgress{Name: name, Intent: intent}
	}
	if t.inFlight.Subagents == nil {
		t.inFlight.Subagents = make(map[string]*SubagentProgress)
	}
	child := &SubagentProgress{ID: name, Name: name, Intent: intent}
	t.inFlight.Subagents[name] = child
	if t.progress != nil {
		t.progress.Mode = StateWorking
	}
	return *child
}

func (t TurnReducer) StartChild(id string) bool {
	if t.inFlight == nil || t.inFlight.Subagents == nil {
		return false
	}
	child, ok := t.inFlight.Subagents[id]
	if !ok {
		return false
	}
	child.Status = "running"
	return true
}

func (t TurnReducer) AppendChildDelta(id string, delta string) bool {
	if t.inFlight == nil || t.inFlight.Subagents == nil {
		return false
	}
	child, ok := t.inFlight.Subagents[id]
	if !ok {
		return false
	}
	child.Output += delta
	return true
}

func (t TurnReducer) CompleteChild(id, output string, ts time.Time) (session.Entry, bool) {
	if t.inFlight == nil || t.inFlight.Subagents == nil {
		return nil, false
	}
	child, ok := t.inFlight.Subagents[id]
	if !ok {
		return nil, false
	}
	delete(t.inFlight.Subagents, id)
	if len(t.inFlight.Subagents) == 0 {
		t.inFlight.Subagents = nil
	}
	if t.progress != nil && len(t.inFlight.Subagents) == 0 {
		t.progress.Mode = StateIonizing
		t.progress.Status = ""
	}
	now := time.Now()
	if !ts.IsZero() {
		now = ts
	}
	return &session.TestEntry{
		Role:      session.RoleSubagent,
		Title:     child.Name,
		Content:   "Completed: " + output,
		Timestamp: now,
	}, true
}

func (t TurnReducer) FailChild(id, reason string, ts time.Time) (session.Entry, bool) {
	if t.inFlight == nil || t.inFlight.Subagents == nil {
		return nil, false
	}
	child, ok := t.inFlight.Subagents[id]
	if !ok {
		return nil, false
	}
	delete(t.inFlight.Subagents, id)
	if len(t.inFlight.Subagents) == 0 {
		t.inFlight.Subagents = nil
	}
	if t.progress != nil {
		t.progress.Mode = StateError
		t.progress.LastError = "Subagent failed: " + reason
	}
	now := time.Now()
	if !ts.IsZero() {
		now = ts
	}
	return &session.TestEntry{
		Role:      session.RoleSubagent,
		Title:     child.Name,
		Content:   "Failed: " + reason,
		IsError:   true,
		Timestamp: now,
	}, true
}
