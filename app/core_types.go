package app

import "strings"

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
	{Name: "/help", Description: "Show help", Idle: -1},
	{Name: "/hotkeys", Description: "Show hotkeys", Idle: -1},
	{Name: "/reload", Description: "Reload configuration"},
	{Name: "/scoped-models", Description: "Show scoped models", Idle: -1},
	{Name: "/primary", Description: "Switch to primary preset"},
	{Name: "/fast", Description: "Switch to fast preset"},
	{Name: "/resume", Description: "Resume a session"},
	{Name: "/model", Description: "Switch model"},
	{Name: "/thinking", Description: "Toggle thinking"},
	{Name: "/provider", Description: "Switch provider"},
	{Name: "/login", Description: "Log in to a provider"},
	{Name: "/logout", Description: "Log out of current provider"},
	{Name: "/settings", Description: "Open settings", Idle: -1},
	{Name: "/tools", Description: "Show tool surface", Idle: -1},
	{Name: "/jobs", Description: "List or stop background jobs", Args: "[stop <job-id>]", Idle: -1},
	{Name: "/memory", Description: "List, search, audit, forget, or restore workspace memory", Args: "[search <query>|audit|forget <id>|restore <id>|all]", Idle: -1},
	{Name: "/rewind", Description: "Preview or restore a workspace checkpoint", Args: "[checkpoint-id [--apply]]", Idle: SlashCommandIdleAlways},
	{Name: "/status", Description: "Show runtime status", Idle: -1},
	{Name: "/changelog", Description: "Show changelog", Idle: -1},
	{Name: "/skills", Description: "List or search skills", Idle: -1},
	{Name: "/new", Description: "Start a new session"},
	{Name: "/clear", Description: "Clear screen"},
	{Name: "/cost", Description: "Show session cost", Idle: -1},
	{Name: "/session", Description: "Show session info", Idle: -1},
	{Name: "/compact", Description: "Compact context"},
	{Name: "/tree", Description: "Show session tree", Idle: -1},
	{Name: "/export", Description: "Export session as JSON", Idle: -1},
	{Name: "/export-html", Description: "Export session as HTML", Idle: -1},
	{Name: "/import", Description: "Import session from JSON"},
	{Name: "/name", Description: "Name the current session"},
	{Name: "/label", Description: "Show or set label on current branch", Args: "[text]", Idle: SlashCommandIdleAlways},
	{Name: "/clone", Description: "Clone current session"},
	{Name: "/copy", Description: "Copy last response", Idle: -1},
	{Name: "/debug", Description: "Show debug info", Idle: -1},
	{Name: "/exit", Description: "Exit ion"},
	{Name: "/quit", Description: "Exit ion"},
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
			Name:      d.Name,
			Detail:    d.Description,
			Available: true,
			Idle:      d.Idle,
		}
	}
	return out
}

func ResolveSlashCommand(name string) (SlashCommandInfo, bool) {
	for _, d := range slashCommandDefs {
		if d.Name == name || d.Name == "/"+name {
			return SlashCommandInfo{
				Name:      d.Name,
				Detail:    d.Description,
				Available: true,
				Idle:      d.Idle,
			}, true
		}
	}
	return SlashCommandInfo{}, false
}

func SlashCommandCatalog() []SlashCommandInfo                 { return SlashCommandDefinitions() }
func LookupSlashCommand(name string) (SlashCommandInfo, bool) { return ResolveSlashCommand(name) }

func ToolEnvironmentLabel(value string) string {
	switch strings.TrimSpace(value) {
	case "":
		return ""
	case "inherit":
		return "inherited"
	case "inherit_without_provider_keys":
		return "inherited without provider keys"
	default:
		return strings.TrimSpace(value)
	}
}

func ToolEnvironmentSummary(value string) string {
	label := ToolEnvironmentLabel(value)
	if label == "" {
		return ""
	}
	return "Bash env " + label
}
