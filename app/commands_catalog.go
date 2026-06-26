package app

import (
	"strings"

)

func helpText() string                                 { return HelpText() }
func hotkeysText() string                              { return HotkeysText() }
func slashCommands() []string                          { return SlashCommands() }
func deferredFeatureMessage(f string) string           { return DeferredFeatureMessage(f) }
func slashCommandDefinitions() []SlashCommandInfo { return SlashCommandDefinitions() }
func slashCommandDefinition(name string) (SlashCommandInfo, bool) {
	return LookupSlashCommand(name)
}
func resolveSlashCommand(name string) (SlashCommandInfo, bool) {
	return ResolveSlashCommand(name)
}
func slashCommandCatalog() []SlashCommandInfo { return SlashCommandCatalog() }
func slashCommandHelpLines() []string              { return SlashCommandHelpLines() }

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
