package app

import (
	"strings"

	"github.com/nijaru/ion/internal/core"
)

func helpText() string                                 { return core.HelpText() }
func slashCommands() []string                          { return core.SlashCommands() }
func deferredFeatureMessage(f string) string           { return core.DeferredFeatureMessage(f) }
func slashCommandDefinitions() []core.SlashCommandInfo { return core.SlashCommandDefinitions() }
func slashCommandDefinition(name string) (core.SlashCommandInfo, bool) {
	return core.SlashCommandDefinition(name)
}
func resolveSlashCommand(name string) (core.SlashCommandInfo, bool) {
	return core.ResolveSlashCommand(name)
}
func slashCommandCatalog() []core.SlashCommandInfo { return core.SlashCommandCatalog() }
func slashCommandHelpLines() []string              { return core.SlashCommandHelpLines() }

// slashCommandItems stays in app/ because it uses pickerItem (TUI type).
func slashCommandItems() []pickerItem {
	commands := slashCommandCatalog()
	items := make([]pickerItem, 0, len(commands))
	for _, command := range commands {
		search := pickerSearchIndex(
			command.Name,
			strings.TrimPrefix(command.Name, "/"),
			command.Detail,
			"Commands",
			nil,
		)
		items = append(items, pickerItem{
			Label:  command.Name,
			Value:  command.Name,
			Detail: command.Detail,
			Group:  "Commands",
			Search: search,
		})
	}
	return items
}
