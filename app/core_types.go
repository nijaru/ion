package app

import (
	"github.com/nijaru/ion/internal/runtime"
)

// Re-export runtime types so app/ code can use them without runtime.X prefix.
type ProgressMode = runtime.ProgressMode
type TurnSummary = runtime.TurnSummary
type SubagentProgress = runtime.SubagentProgress
type InFlightState = runtime.InFlightState
type ProgressState = runtime.ProgressState
type Preset = runtime.Preset
type Snapshot = runtime.Snapshot
type Transition = runtime.Transition
type Accepted = runtime.Accepted
type SetupPromptKind = runtime.SetupPromptKind
type Handles = runtime.Handles
type Switcher = runtime.Switcher
type SwitchInput = runtime.SwitchInput
type SwitchResult = runtime.SwitchResult
type ResumeInput = runtime.ResumeInput
type CancelDecision = runtime.CancelDecision
type StatusChangedDecision = runtime.StatusChangedDecision
type TurnFinishedDispatch = runtime.TurnFinishedDispatch
type ProviderSelection = runtime.ProviderSelection
type SessionStateInfo = runtime.SessionStateInfo
type TurnReducer = runtime.TurnReducer

// Re-export runtime functions
var (
	PresetFromString   = runtime.PresetFromString
	NewSnapshot        = runtime.NewSnapshot
	NewTransition      = runtime.NewTransition
	NewAccepted        = runtime.NewAccepted
	Switch             = runtime.Switch
	Resume             = runtime.Resume
	CloseHandles       = runtime.CloseHandles
	GetSessionState    = runtime.GetSessionState
	IsLocalBusyStatus  = runtime.IsLocalBusyStatus
	IsCompactingStatus = runtime.IsCompactingStatus
	NewTurnReducer     = runtime.NewTurnReducer
)

// --- TUI-specific types ---

type SlashCommandInfo struct {
	Detail    string
	Name      string
	Available bool
	Idle      int
	Deferred  bool
}

const (
	SlashCommandIdleAlways   = 0
	SlashCommandIdleWithArgs = 1
)

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
		out[i] = SlashCommandInfo{Name: d.Name, Detail: d.Description}
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

func SlashCommandCatalog() []SlashCommandInfo    { return SlashCommandDefinitions() }
func LookupSlashCommand(name string) (SlashCommandInfo, bool) { return ResolveSlashCommand(name) }

func SlashCommandHelpLines() []string {
	out := make([]string, len(slashCommandDefs))
	for i, d := range slashCommandDefs {
		out[i] = d.Name + " — " + d.Description
	}
	return out
}
