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
			hidden:     true,
			deferred:   true,
		},
		{
			name:       "/tree",
			detail:     "session tree",
			helpLabel:  "/tree",
			helpDetail: "show session lineage and children",
			hidden:     true,
			deferred:   true,
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
			helpDetail: "show available tools",
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
			name:       "/status",
			detail:     "runtime status",
			helpLabel:  "/status",
			helpDetail: "show runtime, tools, and safety posture",
		},
		{
			name:       "/jobs",
			detail:     "background jobs",
			helpLabel:  "/jobs",
			helpDetail: "show background jobs",
			hidden:     true,
			deferred:   true,
		},
		{
			name:       "/stop",
			detail:     "stop background job",
			helpLabel:  "/stop <job-id>",
			helpDetail: "stop a background job",
			idle:       slashCommandIdleWithArgs,
			hidden:     true,
			deferred:   true,
		},
		{name: "/quit", detail: "quit", helpLabel: "/quit, /exit", helpDetail: "leave ion"},
		{name: "/exit", detail: "quit", hidden: true},

		{
			name:       "/rewind",
			detail:     "restore checkpoint",
			helpLabel:  "/rewind <id>",
			helpDetail: "preview checkpoint restore; add --confirm to apply",
			idle:       slashCommandIdleAlways,
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
