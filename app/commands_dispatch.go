package app

import (
	"fmt"
	"strings"

	"github.com/nijaru/ion/config"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/llm"
)

// handleCommand dispatches a slash command entered by the user.
func (m Model) handleCommand(input string) (Model, tea.Cmd) {
	fields := strings.Fields(input)
	if len(fields) == 0 {
		return m, nil
	}
	command := fields[0]
	if strings.HasPrefix(command, "//") {
		name := strings.TrimPrefix(command, "//")
		if name == "" {
			return m, cmdError("usage: //skill_name")
		}
		return m.handleSkillDetailCommand(name)
	}

	commandInfo, ok := resolveSlashCommand(command)
	if !ok {
		return m, cmdError(fmt.Sprintf("unknown command: %s", command))
	}
	if !commandInfo.Available {
		return m, cmdError(deferredFeatureMessage(commandInfo.Name))
	}
	if m.commandRequiresIdle(commandInfo, fields) && m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage(commandInfo.Name))
	}

	switch commandInfo.Name {
	case "/help":
		return m.handleHelpCommand()
	case "/hotkeys":
		return m.handleHotkeysCommand()
	case "/reload":
		return m.reloadConfig()
	case "/scoped-models":
		return m.showScopedModels()
	case "/primary":
		return m.handlePrimaryCommand(fields)
	case "/fast":
		return m.handleFastCommand(fields)
	case "/resume":
		return m.handleResumeCommand(fields)
	case "/model":
		return m.handleModelCommand(fields)
	case "/thinking":
		return m.handleThinkingCommand(fields)
	case "/provider":
		return m.handleProviderCommandDispatch(fields)
	case "/login":
		return m.handleLoginCommand(fields)
	case "/logout":
		return m.logoutProvider()
	case "/settings":
		return m.handleSettingsCommand(fields)
	case "/tools":
		return m.handleToolsCommand(fields)
	case "/jobs":
		return m.handleJobsCommand(fields)
	case "/memory":
		return m.handleMemoryCommand(fields)
	case "/rewind":
		return m.handleRewindCommand(fields)
	case "/status":
		return m.handleStatusCommand(fields)
	case "/actions":
		return m.handleActionsCommand(fields)
	case "/turns":
		return m.handleTurnsCommand(fields)
	case "/changelog":
		return m.handleChangelogCommand(fields)
	case "/skills":
		return m.handleSkillsCommand(input, command)
	case "/new", "/clear":
		return m.handleNewSessionCommand(fields, command)
	case "/cost":
		return m, m.sessionCostCmd()
	case "/session":
		return m, m.sessionInfoCmd()
	case "/compact":
		return m.handleCompactCommand()
	case "/tree":
		return m.openTreePicker()
	case "/fork":
		return m.handleForkCommand(fields)
	case "/undo":
		return m.handleUndoCommand()
	case "/diff":
		return m.handleDiffCommand(fields)
	case "/export":
		return m.exportSession()
	case "/export-html":
		return m.exportSessionHTML()
	case "/import":
		return m.handleImportCommand(fields)
	case "/search":
		return m.handleSearchCommand(fields)
	case "/name":
		return m.handleNameCommand(fields)
	case "/label":
		return m.handleLabelCommand(fields)
	case "/clone":
		return m.cloneSession()
	case "/copy":
		return m.copyLastResponse()
	case "/debug":
		return m.handleDebugCommand()
	case "/exit", "/quit":
		return m.handleExitCommand()
	default:
		return m, cmdError(fmt.Sprintf("unknown command: %s", fields[0]))
	}
}

func (m Model) commandConfigWithActiveProvider(cfg *config.Config) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	provider := m.runtimeProvider()
	if strings.TrimSpace(cfg.Provider) != "" || provider == "" {
		return cfg
	}

	def, ok := llm.Lookup(provider)
	if !ok {
		return cfg
	}
	if def.ID == llm.OpenAICompatibleID && strings.TrimSpace(cfg.Endpoint) == "" {
		return cfg
	}

	updated := *cfg
	updated.Provider = def.ID
	return &updated
}

func (m Model) localCommandBusy() bool {
	return m.InFlight.Thinking ||
		m.InFlight.AwaitingSettlement ||
		m.Progress.Compacting ||
		m.Model.RuntimeSwitchRequest != 0 ||
		m.Picker.SetupSaveRequest != 0 ||
		m.Model.SettingsRequest != 0 ||
		m.Model.RecoveryRequest != 0 ||
		m.Model.InterruptedTurnRequest != 0
}

func (m Model) localCommandBusyMessage(action string) string {
	if m.Model.RuntimeSwitchRequest != 0 {
		return "Wait for the runtime switch to finish before " + action + "."
	}
	if m.Picker.SetupSaveRequest != 0 {
		return "Wait for provider setup to finish before " + action + "."
	}
	if m.Model.SettingsRequest != 0 {
		return "Wait for settings to finish before " + action + "."
	}
	if m.Model.RecoveryRequest != 0 {
		return "Wait for action reconciliation to finish before " + action + "."
	}
	if m.Model.InterruptedTurnRequest != 0 {
		return "Wait for interrupted-turn recovery to finish before " + action + "."
	}
	return "Finish or cancel the current turn before " + action + "."
}

func (m Model) commandRequiresIdle(command SlashCommandInfo, fields []string) bool {
	switch command.Idle {
	case SlashCommandIdleAlways:
		return true
	case SlashCommandIdleWithArgs:
		return len(fields) > 1
	default:
		return false
	}
}

// cmdError returns a Cmd that emits a local UI error with the given message.
func cmdError(msg string) tea.Cmd {
	return func() tea.Msg {
		return localErrorMsg{err: fmt.Errorf("%s", msg)}
	}
}

func helpText() string                       { return HelpText() }
func hotkeysText() string                    { return HotkeysText() }
func slashCommands() []string                { return SlashCommands() }
func deferredFeatureMessage(f string) string { return DeferredFeatureMessage(f) }
func resolveSlashCommand(name string) (SlashCommandInfo, bool) {
	return ResolveSlashCommand(name)
}
func slashCommandCatalog() []SlashCommandInfo { return SlashCommandCatalog() }

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
