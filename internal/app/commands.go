package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	ionskills "github.com/nijaru/ion/internal/skills"
	"github.com/nijaru/ion/internal/storage"
)

// handleCommand dispatches a slash command entered by the user.
func (m Model) handleCommand(input string) (Model, tea.Cmd) {
	fields := strings.Fields(input)
	if len(fields) == 0 {
		return m, nil
	}
	command := fields[0]
	commandInfo, ok := slashCommandDefinition(command)
	if !ok {
		return m, cmdError(fmt.Sprintf("unknown command: %s", command))
	}
	if !commandInfo.available() {
		return m, cmdError(deferredFeatureMessage(command))
	}
	if m.commandRequiresIdle(commandInfo, fields) && m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage(command))
	}

	switch command {
	case "/help":
		return m, m.terminalCommit().Help(helpText())

	case "/primary":
		if len(fields) != 1 {
			return m, cmdError("usage: /primary")
		}
		return m.switchPresetCommand(presetPrimary)

	case "/fast":
		if len(fields) != 1 {
			return m, cmdError("usage: /fast")
		}
		return m.switchPresetCommand(presetFast)

	case "/resume":
		if len(fields) < 2 {
			return m.openSessionPicker()
		}
		return m.resumeStoredSessionByID(fields[1])
	case "/model":
		if len(fields) < 2 {
			return m.openModelPicker()
		}
		name := strings.Join(fields[1:], " ")
		cfg, err := m.commandConfig()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		cfg = m.commandConfigWithActiveProvider(cfg)
		currentCfg, err := m.runtimeConfigForActivePreset(cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" &&
			strings.EqualFold(strings.TrimSpace(currentCfg.Model), strings.TrimSpace(name)) {
			return m, nil
		}
		transition, runtimeCfg, err := m.modelSelectionTransition(
			cfg,
			m.activePreset(),
			name,
		)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if runtimeCfg.Provider == "" {
			return m, cmdError("cannot set model without an active provider; use /provider first")
		}
		return m.switchRuntimeCommand(
			transition,
			systemEntry("Model set to "+name),
			m.currentMaterializedSessionID(),
			false,
		)

	case "/thinking":
		if len(fields) < 2 {
			return m.openThinkingPicker()
		}
		level := normalizeThinkingValue(fields[1])
		cfg, err := m.commandConfig()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		currentCfg, err := m.runtimeConfigForActivePreset(cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" &&
			normalizeThinkingValue(currentCfg.ReasoningEffort) == level {
			return m, nil
		}
		transition, _, err := m.thinkingSelectionTransition(
			cfg,
			m.activePreset(),
			level,
		)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		return m.beginRuntimeTransitionCommit(
			transition,
			systemEntry("Thinking set to "+thinkingDisplayName(level)),
		)

	case "/provider":
		if len(fields) < 2 {
			return m.openProviderPicker()
		}
		name := fields[1]
		cfg, err := m.commandConfig()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		return m.beginProviderSelection(cfg, name, m.activePreset())

	case "/login":
		cfg, err := m.commandConfig()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		provider := ""
		if len(fields) >= 2 {
			provider = fields[1]
		} else {
			provider = cfg.Provider
		}
		if strings.TrimSpace(provider) == "" {
			return m.openProviderPicker()
		}
		return m.openAPIKeyPrompt(cfgForProvider(cfg, provider), provider, m.activePreset())

	case "/settings":
		return m.handleSettingsCommand(fields)

	case "/tools":
		if len(fields) != 1 {
			return m, cmdError("usage: /tools")
		}
		summarizer, ok := m.Model.Backend.(backend.ToolSummarizer)
		if !ok {
			return m, cmdError("tool summary unavailable for this backend")
		}
		surface := summarizer.ToolSurface()
		return m, m.terminalCommit().Entries(
			systemEntry(toolSurfaceSummary(surface)),
		)

	case "/status":
		if len(fields) != 1 {
			return m, cmdError("usage: /status")
		}
		return m, m.terminalCommit().Entries(
			systemEntry(runtimeStatusSummary(m)),
		)

	case "/skills":
		dir, err := config.DefaultSkillsDir()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve skills dir: %v", err))
		}
		query := strings.TrimSpace(strings.TrimPrefix(input, command))
		out, err := ionskills.Notice([]string{dir}, query)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load skills: %v", err))
		}
		return m, m.terminalCommit().Entries(systemEntry(out))

	case "/new", "/clear":
		cfg, err := m.commandConfig()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		runtimeCfg, err := m.runtimeConfigForActivePreset(cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if runtimeCfg.Provider == "" {
			runtimeCfg.Provider = m.runtimeProvider()
		}
		if runtimeCfg.Model == "" {
			runtimeCfg.Model = m.runtimeModel()
		}
		if runtimeCfg.Provider == "" || runtimeCfg.Model == "" {
			return m, cmdError("cannot " + command + " without an active provider and model")
		}
		appCfg := cfg
		if appCfg == nil {
			appCfg = &config.Config{}
		}
		if strings.TrimSpace(appCfg.Provider) == "" {
			updated := *appCfg
			updated.Provider = runtimeCfg.Provider
			appCfg = &updated
		}
		if configuredModelForPreset(appCfg, m.activePreset()) == "" {
			appCfg = updateModelForPreset(appCfg, runtimeCfg.Model, m.activePreset())
		}
		notice := "Started new session"
		if command == "/clear" {
			notice = "Started fresh session"
		}
		transition := newRuntimeTransition(appCfg, runtimeCfg, m.activePreset(), "")
		return m.switchRuntimeCommand(
			transition,
			systemEntry(notice),
			"",
			false,
		)

	case "/cost":
		return m, m.sessionCostCmd()

	case "/session":
		return m, m.sessionInfoCmd()

	case "/compact":
		if m.Model.Storage != nil && !storage.IsMaterialized(m.Model.Storage) {
			return m, m.terminalCommit().Entries(systemEntry("No active session to compact yet"))
		}
		compactor, ok := m.Model.Backend.(backend.Compactor)
		if !ok {
			return m, cmdError("current backend does not support /compact")
		}
		m.progressReducer().beginCompaction()
		return m, func() tea.Msg {
			compacted, err := compactor.Compact(context.Background())
			if err != nil {
				return localErrorMsg{err: err}
			}
			if compacted {
				return sessionCompactedMsg{notice: "Compacted current session context"}
			}
			return sessionCompactedMsg{notice: "Session is already within compaction limits"}
		}

	case "/exit", "/quit":
		return m, tea.Quit

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

	def, ok := providers.Lookup(provider)
	if !ok || def.Runtime != providers.RuntimeNative {
		return cfg
	}
	if def.ID == providers.OpenAICompatibleID && strings.TrimSpace(cfg.Endpoint) == "" {
		return cfg
	}

	updated := *cfg
	updated.Provider = def.ID
	return &updated
}

func (m Model) localCommandBusy() bool {
	return m.InFlight.Thinking ||
		m.Progress.Compacting ||
		m.Model.RuntimeSwitchRequest != 0 ||
		m.Picker.ProviderSelectionRequest != 0 ||
		m.Picker.SetupSaveRequest != 0 ||
		m.Model.SettingsRequest != 0
}

func (m Model) localCommandBusyMessage(action string) string {
	if m.Model.RuntimeSwitchRequest != 0 {
		return "Wait for the runtime switch to finish before " + action + "."
	}
	if m.Picker.ProviderSelectionRequest != 0 {
		return "Wait for the provider check to finish before " + action + "."
	}
	if m.Picker.SetupSaveRequest != 0 {
		return "Wait for provider setup to finish before " + action + "."
	}
	if m.Model.SettingsRequest != 0 {
		return "Wait for settings to finish before " + action + "."
	}
	return "Finish or cancel the current turn before " + action + "."
}

func (m Model) commandRequiresIdle(command slashCommandInfo, fields []string) bool {
	switch command.idle {
	case slashCommandIdleAlways:
		return true
	case slashCommandIdleWithArgs:
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
