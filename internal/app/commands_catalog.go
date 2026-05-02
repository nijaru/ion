package app

import (
	"fmt"
	"strings"
)

func helpText() string {
	lines := []string{
		"ion commands",
		"",
	}
	lines = append(lines, slashCommandHelpLines()...)
	lines = append(lines,
		"",
		"keys",
		"",
		"  Ctrl+M           toggle configured primary/fast preset",
		"  Ctrl+T           thinking picker",
		"  Tab              complete slash commands and @file refs; swap provider/model pickers",
		"  PgUp / PgDn      page through picker lists",
		"  Shift+Tab        toggle READ <-> EDIT",
		"  Esc              cancel running turn",
		"  Up / Down        command history",
		"  Ctrl+P / Ctrl+N  command history",
		"  Enter            send message",
		"  Shift+Enter      insert newline",
		"  Alt+Enter        insert newline",
		"  Ctrl+C           clear composer, or quit on double-tap when empty",
		"  Ctrl+D           quit on double-tap when empty",
	)
	return strings.Join(lines, "\n")
}

func slashCommands() []string {
	commands := slashCommandCatalog()
	out := make([]string, 0, len(commands))
	for _, command := range commands {
		out = append(out, command.name)
	}
	return out
}

type slashCommandInfo struct {
	name       string
	detail     string
	helpLabel  string
	helpDetail string
	idle       slashCommandIdlePolicy
	hidden     bool
	deferred   bool
}

type slashCommandIdlePolicy int

const (
	slashCommandIdleNever slashCommandIdlePolicy = iota
	slashCommandIdleAlways
	slashCommandIdleWithArgs
)

func (c slashCommandInfo) available() bool {
	return !c.deferred
}

func deferredFeatureMessage(feature string) string {
	return feature + " is deferred until its roadmap phase"
}

func slashCommandDefinitions() []slashCommandInfo {
	return []slashCommandInfo{
		{name: "/help", detail: "show help", helpLabel: "/help", helpDetail: "show this help"},
		{
			name:       "/new",
			detail:     "start a new session",
			helpLabel:  "/new",
			helpDetail: "start a fresh session",
			idle:       slashCommandIdleAlways,
		},
		{
			name:       "/clear",
			detail:     "start a fresh session",
			helpLabel:  "/clear",
			helpDetail: "start a fresh session with the current provider/model",
			idle:       slashCommandIdleAlways,
		},
		{
			name:       "/resume",
			detail:     "resume a recent session",
			helpLabel:  "/resume [id]",
			helpDetail: "resume a recent session or pick one",
			idle:       slashCommandIdleAlways,
		},
		{
			name:       "/session",
			detail:     "current session info",
			helpLabel:  "/session",
			helpDetail: "show current session info",
		},
		{
			name:       "/fork",
			detail:     "fork current session",
			helpLabel:  "/fork [label]",
			helpDetail: "branch the current session",
			idle:       slashCommandIdleAlways,
		},
		{
			name:       "/compact",
			detail:     "compact session",
			helpLabel:  "/compact",
			helpDetail: "compact the current session",
			idle:       slashCommandIdleAlways,
		},

		{
			name:       "/provider",
			detail:     "choose provider",
			helpLabel:  "/provider [name]",
			helpDetail: "set provider and choose a model",
			idle:       slashCommandIdleWithArgs,
		},
		{
			name:       "/model",
			detail:     "choose model",
			helpLabel:  "/model [name]",
			helpDetail: "set model directly or open the picker",
			idle:       slashCommandIdleWithArgs,
		},
		{
			name:       "/thinking",
			detail:     "choose thinking level",
			helpLabel:  "/thinking [lvl]",
			helpDetail: "set thinking: auto, off, minimal, low, medium, high, xhigh",
			idle:       slashCommandIdleWithArgs,
		},
		{
			name:       "/primary",
			detail:     "switch to primary preset",
			helpLabel:  "/primary",
			helpDetail: "switch to the primary model preset",
			idle:       slashCommandIdleAlways,
		},
		{
			name:       "/fast",
			detail:     "switch to fast preset",
			helpLabel:  "/fast",
			helpDetail: "switch to the configured fast model preset",
			idle:       slashCommandIdleAlways,
		},

		{
			name:       "/settings",
			detail:     "common settings",
			helpLabel:  "/settings",
			helpDetail: "show or change common settings",
			idle:       slashCommandIdleWithArgs,
		},
		{
			name:       "/tools",
			detail:     "tool status",
			helpLabel:  "/tools",
			helpDetail: "show tool count and lazy loading status",
		},
		{
			name:       "/skills",
			detail:     "installed skills",
			helpLabel:  "/skills [query]",
			helpDetail: "show installed local skills",
		},
		{
			name:       "/cost",
			detail:     "session usage",
			helpLabel:  "/cost",
			helpDetail: "show aggregate session usage",
		},
		{
			name:       "/mode",
			detail:     "set read/edit/auto",
			helpLabel:  "/mode [mode]",
			helpDetail: "set mode: read, edit, auto",
		},
		{name: "/read", detail: "READ mode", hidden: true},
		{name: "/edit", detail: "EDIT mode", hidden: true},
		{name: "/auto", detail: "AUTO mode", hidden: true},
		{name: "/yolo", detail: "AUTO mode alias", hidden: true},

		{name: "/quit", detail: "quit", helpLabel: "/quit, /exit", helpDetail: "leave ion"},
		{name: "/exit", detail: "quit", hidden: true},

		{
			name:       "/trust",
			detail:     "workspace trust",
			helpLabel:  "/trust [status]",
			helpDetail: "trust this workspace or show trust status",
		},
		{
			name:       "/rewind",
			detail:     "restore checkpoint",
			helpLabel:  "/rewind <id>",
			helpDetail: "preview checkpoint restore; add --confirm to apply",
			idle:       slashCommandIdleAlways,
			deferred:   true,
		},
		{
			name:       "/memory",
			detail:     "memory search",
			helpLabel:  "/memory [query]",
			helpDetail: "show workspace memory tree or search memory",
			deferred:   true,
		},
		{
			name:       "/mcp",
			detail:     "register MCP server",
			helpLabel:  "/mcp add <cmd>",
			helpDetail: "register an MCP server",
			deferred:   true,
		},
	}
}

func slashCommandDefinition(name string) (slashCommandInfo, bool) {
	for _, command := range slashCommandDefinitions() {
		if command.name == name {
			return command, true
		}
	}
	return slashCommandInfo{}, false
}

func slashCommandCatalog() []slashCommandInfo {
	definitions := slashCommandDefinitions()
	commands := make([]slashCommandInfo, 0, len(definitions))
	for _, command := range definitions {
		if command.hidden || !command.available() {
			continue
		}
		commands = append(commands, command)
	}
	return commands
}

func slashCommandHelpLines() []string {
	commands := slashCommandCatalog()
	lines := make([]string, 0, len(commands))
	for _, command := range commands {
		if command.helpLabel == "" {
			continue
		}
		lines = append(lines, fmt.Sprintf("  %-16s %s", command.helpLabel, command.helpDetail))
	}
	return lines
}

func slashCommandItems() []pickerItem {
	commands := slashCommandCatalog()
	items := make([]pickerItem, 0, len(commands))
	for _, command := range commands {
		search := pickerSearchIndex(
			command.name,
			strings.TrimPrefix(command.name, "/"),
			command.detail,
			"Commands",
			nil,
		)
		items = append(items, pickerItem{
			Label:  command.name,
			Value:  command.name,
			Detail: command.detail,
			Group:  "Commands",
			Search: search,
		})
	}
	return items
}
