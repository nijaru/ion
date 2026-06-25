package core

import (
	"fmt"
	"context"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

// progressMode tracks the current turn lifecycle phase.
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
	PendingTools            map[string]*session.Entry
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

// App-facing types — will move to app/ during its rewrite.
// These are real domain types, not transitional stubs.

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

type Transition struct {
	Snapshot             Snapshot
	PersistState         bool
	PersistReasoning     bool
	PersistActivePreset  bool
	PersistReasoningSlot Preset
}

type Accepted struct {
	Transition Transition
	Handles    Handles
}

func NewAccepted(transition Transition, handles Handles) Accepted {
	return Accepted{Transition: transition, Handles: handles}
}

type SetupPromptKind int

const (
	SetupPromptAPIKey SetupPromptKind = iota + 1
	SetupPromptEndpoint
)

type Handles struct {
	Backend Backend
	Session session.Session
	Storage session.Session
}

type Switcher func(context.Context, *config.Config, string) (Backend, session.Session, session.Session, error)

type SlashCommandInfo struct {
	Detail string
	Name      string
	Available bool
	Idle      int
	Deferred  bool
}

const (
	SlashCommandIdleAlways   = 0
	SlashCommandIdleWithArgs = 1
)

type TurnReducer struct{}

type ProviderSelection struct {
	Setup SetupPromptKind
	Config               *config.Config
	SupportsModelListing bool
	Transition           Transition
}



type SlashCommandDefinition struct {
	Name        string
	Description string
	Detail      string
	Args        string
	Idle        int
}

var slashCommandDefs = []SlashCommandDefinition{
	{Name: "/help", Description: "Show help"},
	{Name: "/compact", Description: "Compact context"},
	{Name: "/clear", Description: "Clear screen"},
	{Name: "/model", Description: "Switch model"},
	{Name: "/thinking", Description: "Toggle thinking"},
	{Name: "/export-html", Description: "Export session as HTML"},
	{Name: "/import-json", Description: "Import session from JSON"},
	{Name: "/copy", Description: "Copy last response"},
	{Name: "/tree", Description: "Show session tree"},
}

func HelpText() string                       { return "Type /help for commands" }
func HotkeysText() string                    { return "Ctrl+C: cancel, Ctrl+D: exit" }
func DeferredFeatureMessage(f string) string { return "Feature not yet available: " + f }
func SlashCommands() []string {
	out := make([]string, len(slashCommandDefs))
	for i, d := range slashCommandDefs {
		out[i] = d.Name
	}
	return out
}

func SlashCommandDefinitions() []SlashCommandInfo {
	out := make([]SlashCommandInfo, len(slashCommandDefs))
	for i, d := range slashCommandDefs {
		out[i] = SlashCommandInfo{
			Name:   d.Name,
			Detail: d.Description,
		}
	}
	return out
}

func ResolveSlashCommand(name string) (SlashCommandInfo, bool) {
	for _, d := range slashCommandDefs {
		if d.Name == name || d.Name == "/"+name {
			return SlashCommandInfo{Name: d.Name, Detail: d.Description}, true
		}
	}
	return SlashCommandInfo{}, false
}

func SlashCommandCatalog() []SlashCommandInfo {
	return SlashCommandDefinitions()
}

func SlashCommandHelpLines() []string {
	out := make([]string, len(slashCommandDefs))
	for i, d := range slashCommandDefs {
		out[i] = d.Name + " — " + d.Description
	}
	return out
}

// Additional core types used by app/.






type SessionStateInfo struct {
	Backend Backend
	Session session.Session
	Store   session.Store
}




func IsLocalBusyStatus(s string) bool {
	return s != "" && s != "idle" && s != "complete"
}

func IsCompactingStatus(s string) bool {
	return s == "compacting"
}

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

func Resume(ctx context.Context, input ResumeInput) (SwitchResult, error) {
	return SwitchResult{}, fmt.Errorf("resume not implemented")
}

type ResumeInput struct {
	Switcher   Switcher
	Transition Transition
	Current    Handles
	SaveState  func(update config.RuntimeStateUpdate) error
	Context   context.Context
	Store     session.Store
	SessionID string
}

func CloseHandles(handles Handles) {
	// cleanup
}

func Switch(ctx context.Context, input SwitchInput) (SwitchResult, error) {
	return SwitchResult{}, fmt.Errorf("switch not implemented")
}

func LookupSlashCommand(name string) (SlashCommandInfo, bool) {
	return ResolveSlashCommand(name)
}

func (p Preset) String() string { return string(p) }

func (t Transition) WithActivePresetPersistence(p ...Preset) Transition {
	t.PersistActivePreset = true
	t.PersistReasoningSlot = t.Snapshot.Preset
	return t
}

type SwitchResult struct {
	Runtime  Accepted
	Previous Handles
}



func (t TurnReducer) ClearActiveState(full bool)     {}
func (t TurnReducer) ResetFinishedTurnSummary()       {}
func (t TurnReducer) setReasoningEffort(v int)        {}
func (t TurnReducer) applySessionUsage(in, out int, cost float64) {}

func (r SwitchResult) GetEntries(ctx context.Context, store session.Store) ([]session.Entry, error) {
	if store == nil {
		return nil, nil
	}
	return store.Entries(ctx)
}

func (t TurnReducer) PopQueuedTurn() string { return "" }

func (s Snapshot) WithHandles(h Handles) Snapshot { return s }

func NewSnapshot(appCfg, backendCfg *config.Config, preset Preset, status string) Snapshot {
	return Snapshot{Preset: preset, Status: status}
}

func NewTransition(appCfg, backendCfg *config.Config, preset Preset, status string) Transition {
	return Transition{}
}

func (t Transition) NeedsPersistence() bool { return t.PersistState || t.PersistActivePreset }
func (t Transition) WithHandles(h Handles) Transition { return t }

func (t Transition) Persist(fn func(update config.RuntimeStateUpdate) error) error {
	if fn == nil { return nil }
	return fn(config.RuntimeStateUpdate{})
}
func (t Transition) WithStatePersistence() Transition { t.PersistState = true; return t }
func (t Transition) WithReasoningPersistence() Transition { t.PersistReasoning = true; return t }


func GetSessionState(h Handles) (string, bool) {
	if h.Session == nil {
		return "", false
	}
	return h.Session.ID(), true
}

func (t TurnReducer) StartSubmit() {}
func (t TurnReducer) RejectSubmit(reason string) {}

func (t TurnReducer) SetBackendQueuedInput(steering []string, followUp []string) {}
func (t TurnReducer) QueueTurn(text string) {}

func (t TurnReducer) ClearQueuedTurns()        {}
func (t TurnReducer) CancelActiveTurn(reason string, now time.Time) error { return nil }

func (t TurnReducer) DrainingUntilTurnStarted() bool { return false }

type CancelDecision struct {
	EntryContent string
}

func (t TurnReducer) CancelTurn(reason string, now time.Time) CancelDecision {
	return CancelDecision{EntryContent: "Turn cancelled: " + reason}
}

func (t TurnReducer) FinishDrain() {}

func (t TurnReducer) StreamClosed(now time.Time) (session.Entry, bool) { return nil, false }
func (t TurnReducer) FailTurn(msg string, now time.Time)  {}
func (t TurnReducer) ClearLocalErrorIfIdle()              {}
func (t TurnReducer) ApplyStatusChangedInput(msg interface{}) StatusChangedDecision { return StatusChangedDecision{} }

type StatusChangedDecision struct {
	Root            bool
	PersistTimestamp time.Time
	Status          string
}


func (t TurnReducer) StartTurn(now time.Time, ts time.Time)           {}
func (t TurnReducer) StopThinking()                      {}
func (t TurnReducer) FinishPendingAssistant() (session.Entry, bool, bool) { return nil, false, false }
func (t TurnReducer) RecordFinishedTurnSummary(now time.Time) {}
func (t TurnReducer) BeginDrain(now time.Time)           {}

func (t TurnReducer) FinishTurnMode(completed bool) (session.Entry, bool) { return nil, false }


type TurnFinishedDispatch struct {
	ReloadGitDiff     bool
	AwaitNext         bool
	Action              string
	Text                string
	RearmSessionEvents  bool
}

var TurnFinishedDispatchSubmitLocal = "submit_local"

func (t TurnReducer) FinishTurnDispatch() TurnFinishedDispatch {
	return TurnFinishedDispatch{}
}
// ReloadGitDiff and AwaitNext were missing from TurnFinishedDispatch
func init() {}
func (t TurnReducer) ApplyTokenUsage(msg interface{}) {}

func (t TurnReducer) AppendThinkingDelta(agentID string, delta interface{}) {}
func (t TurnReducer) AppendAgentDelta(agentID string, delta interface{}, ts time.Time) {}

func (t TurnReducer) ApplyBudgetStop(reason string, ts time.Time) (session.Entry, error) { return nil, nil }
func (t TurnReducer) CommitAgentMessage(msg interface{}) (session.Entry, bool) { return nil, false }
func (t TurnReducer) StartToolCall(id string, ts time.Time, title string) {}
func (t TurnReducer) AppendToolOutput(id string, output string, isError bool) {}
func (t TurnReducer) AppendToolError(id, name, err string, ts time.Time) {}

func (t TurnReducer) CompleteToolResult(id string, msg interface{}) (session.Entry, bool) { return nil, false }

func NewTurnReducer(inFlight *InFlightState, progress *ProgressState) TurnReducer { return TurnReducer{} }

func (t TurnReducer) AgentStreamContent() string { return "" }
