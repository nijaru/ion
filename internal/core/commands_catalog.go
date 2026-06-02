package core

import (
	"fmt"
	"strings"
)

// SlashCommandInfo describes a slash command and its help text.
type SlashCommandInfo struct {
	Name       string
	Detail     string
	HelpLabel  string
	HelpDetail string
	Idle       SlashCommandIdlePolicy
	Hidden     bool
	Deferred   bool
}

// SlashCommandIdlePolicy controls when a slash command is available.
type SlashCommandIdlePolicy int

const (
	SlashCommandIdleNever SlashCommandIdlePolicy = iota
	SlashCommandIdleAlways
	SlashCommandIdleWithArgs
)

func (c SlashCommandInfo) Available() bool {
	return !c.Deferred
}

// DeferredFeatureMessage returns a standard message for deferred features.
func DeferredFeatureMessage(feature string) string {
	return feature + " is deferred until its roadmap phase"
}

// SlashCommandDefinitions returns the full catalog of slash commands.
func SlashCommandDefinitions() []SlashCommandInfo {
	return []SlashCommandInfo{
		{Name: "/help", Detail: "show help", HelpLabel: "/help", HelpDetail: "show this help"},
		{
			Name:       "/new",
			Detail:     "start a new session",
			HelpLabel:  "/new",
			HelpDetail: "start a fresh session",
			Idle:       SlashCommandIdleAlways,
		},
		{
			Name:       "/clear",
			Detail:     "start a fresh session",
			HelpLabel:  "/clear",
			HelpDetail: "start a fresh session with the current provider/model",
			Idle:       SlashCommandIdleAlways,
		},
		{
			Name:       "/resume",
			Detail:     "resume a recent session",
			HelpLabel:  "/resume [id]",
			HelpDetail: "resume a recent session or pick one",
			Idle:       SlashCommandIdleAlways,
		},
		{
			Name:       "/session",
			Detail:     "current session info",
			HelpLabel:  "/session",
			HelpDetail: "show current session info",
		},
		{
			Name:       "/fork",
			Detail:     "fork current session",
			HelpLabel:  "/fork [label]",
			HelpDetail: "branch the current session",
			Idle:       SlashCommandIdleAlways,
			Hidden:     true,
			Deferred:   true,
		},
		{
			Name:       "/tree",
			Detail:     "session tree",
			HelpLabel:  "/tree",
			HelpDetail: "show session lineage and children",
			Hidden:     true,
			Deferred:   true,
		},
		{
			Name:       "/compact",
			Detail:     "compact session",
			HelpLabel:  "/compact",
			HelpDetail: "compact the current session",
			Idle:       SlashCommandIdleAlways,
		},
		{
			Name:       "/provider",
			Detail:     "choose provider",
			HelpLabel:  "/provider [name]",
			HelpDetail: "set provider and choose a model",
			Idle:       SlashCommandIdleWithArgs,
		},
		{
			Name:       "/login",
			Detail:     "save provider API key",
			HelpLabel:  "/login [provider]",
			HelpDetail: "save an API key for a provider",
			Idle:       SlashCommandIdleWithArgs,
		},
		{
			Name:       "/model",
			Detail:     "choose model",
			HelpLabel:  "/model [name]",
			HelpDetail: "set model directly or open the picker",
			Idle:       SlashCommandIdleWithArgs,
		},
		{
			Name:       "/thinking",
			Detail:     "choose thinking level",
			HelpLabel:  "/thinking [lvl]",
			HelpDetail: "set thinking: auto, off, minimal, low, medium, high, xhigh",
			Idle:       SlashCommandIdleWithArgs,
		},
		{
			Name:       "/primary",
			Detail:     "switch to primary preset",
			HelpLabel:  "/primary",
			HelpDetail: "switch to the primary model preset",
			Idle:       SlashCommandIdleAlways,
		},
		{
			Name:       "/fast",
			Detail:     "switch to fast preset",
			HelpLabel:  "/fast",
			HelpDetail: "switch to the configured fast model preset",
			Idle:       SlashCommandIdleAlways,
		},
		{
			Name:       "/settings",
			Detail:     "common settings",
			HelpLabel:  "/settings",
			HelpDetail: "open common settings",
		},
		{
			Name:       "/tools",
			Detail:     "tool status",
			HelpLabel:  "/tools",
			HelpDetail: "show available tools",
		},
		{
			Name:       "/skills",
			Detail:     "installed skills",
			HelpLabel:  "/skills [query]",
			HelpDetail: "show installed local skills",
		},
		{
			Name:       "/cost",
			Detail:     "session usage",
			HelpLabel:  "/cost",
			HelpDetail: "show aggregate session usage",
		},
		{
			Name:       "/status",
			Detail:     "runtime status",
			HelpLabel:  "/status",
			HelpDetail: "show runtime, tools, and safety posture",
		},
		{
			Name:       "/jobs",
			Detail:     "background jobs",
			HelpLabel:  "/jobs",
			HelpDetail: "show background jobs",
			Hidden:     true,
			Deferred:   true,
		},
		{
			Name:       "/stop",
			Detail:     "stop background job",
			HelpLabel:  "/stop <job-id>",
			HelpDetail: "stop a background job",
			Idle:       SlashCommandIdleWithArgs,
			Hidden:     true,
			Deferred:   true,
		},
		{Name: "/quit", Detail: "quit", HelpLabel: "/quit, /exit", HelpDetail: "leave ion"},
		{Name: "/exit", Detail: "quit", Hidden: true},
		{
			Name:       "/rewind",
			Detail:     "restore checkpoint",
			HelpLabel:  "/rewind <id>",
			HelpDetail: "preview checkpoint restore; add --confirm to apply",
			Idle:       SlashCommandIdleAlways,
			Deferred:   true,
		},
		{
			Name:       "/mcp",
			Detail:     "register MCP server",
			HelpLabel:  "/mcp add <cmd>",
			HelpDetail: "register an MCP server",
			Deferred:   true,
		},
	}
}

// SlashCommandDefinition looks up a command by exact name.
func SlashCommandDefinition(name string) (SlashCommandInfo, bool) {
	for _, command := range SlashCommandDefinitions() {
		if command.Name == name {
			return command, true
		}
	}
	return SlashCommandInfo{}, false
}

// ResolveSlashCommand resolves a user-typed command name to its definition.
func ResolveSlashCommand(name string) (SlashCommandInfo, bool) {
	switch name {
	case "/mode", "/read", "/edit", "/auto", "/yolo", "/trust":
		return SlashCommandInfo{}, false
	}

	if info, ok := SlashCommandDefinition(name); ok {
		return info, true
	}

	var matches []SlashCommandInfo
	for _, command := range SlashCommandCatalog() {
		if strings.HasPrefix(command.Name, name) {
			matches = append(matches, command)
		}
	}
	if len(matches) == 1 {
		return matches[0], true
	}
	return SlashCommandInfo{}, false
}

// SlashCommandCatalog returns the visible, available slash commands.
func SlashCommandCatalog() []SlashCommandInfo {
	definitions := SlashCommandDefinitions()
	commands := make([]SlashCommandInfo, 0, len(definitions))
	for _, command := range definitions {
		if command.Hidden || !command.Available() {
			continue
		}
		commands = append(commands, command)
	}
	return commands
}

// SlashCommandHelpLines returns formatted help text lines for each command.
func SlashCommandHelpLines() []string {
	commands := SlashCommandCatalog()
	lines := make([]string, 0, len(commands))
	for _, command := range commands {
		if command.HelpLabel == "" {
			continue
		}
		lines = append(lines, fmt.Sprintf("  %-16s %s", command.HelpLabel, command.HelpDetail))
	}
	return lines
}

// HelpText returns the full help text for ion.
func HelpText() string {
	lines := []string{
		"ion commands",
		"",
	}
	lines = append(lines, SlashCommandHelpLines()...)
	lines = append(
		lines,
		"",
		"keys",
		"",
		"  Ctrl+M           toggle primary/fast model",
		"  Ctrl+T           thinking picker",
		"  Ctrl+X           open composer in external editor",
		"  Tab              complete slash commands and @file refs; swap provider/model pickers",
		"  PgUp / PgDn      page through picker lists",
		"  Esc              cancel running turn",
		"  Up / Down        command history",
		"  Ctrl+P / Ctrl+N  command history",
		"  Enter            send message",
		"  Ctrl+J           insert newline",
		"  Shift+Enter      insert newline",
		"  Alt+Enter        insert newline",
		"  Ctrl+C           clear composer, cancel running turn, or quit on double-tap when empty",
		"  Ctrl+D           delete forward, or quit on double-tap when empty",
	)
	return strings.Join(lines, "\n")
}

// SlashCommands returns the list of visible slash command names.
func SlashCommands() []string {
	commands := SlashCommandCatalog()
	out := make([]string, 0, len(commands))
	for _, command := range commands {
		out = append(out, command.Name)
	}
	return out
}
