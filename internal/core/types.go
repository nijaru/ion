package core

import (
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

type Snapshot struct{}

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
	Storage session.Store
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
