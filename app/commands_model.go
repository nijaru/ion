package app

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/nijaru/ion/config"

	tea "charm.land/bubbletea/v2"
)

func (m Model) reloadConfig() (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("reloading configuration"))
	}

	requestID := m.runtimeRequest().begin("Reloading configuration...")
	loadConfig := ConfigLoader(config.Load)
	if m.Model.ConfigLoader != nil {
		loadConfig = m.Model.ConfigLoader
	}
	return m, func() tea.Msg {
		km, err := LoadKeybindings()
		if err != nil {
			return reloadConfigLoadedMsg{
				requestID: requestID,
				err:       fmt.Errorf("reload keybindings: %w", err),
			}
		}
		cfg, err := loadConfig()
		if err != nil {
			return reloadConfigLoadedMsg{
				requestID: requestID,
				err:       fmt.Errorf("reload config: %w", err),
			}
		}
		return reloadConfigLoadedMsg{
			requestID:   requestID,
			keybindings: km,
			cfg:         cfg,
		}
	}
}

func (m Model) handleReloadConfigLoaded(msg reloadConfigLoadedMsg) (Model, tea.Cmd) {
	if !m.runtimeRequest().matches(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		m.runtimeRequest().finish(msg.requestID)
		return m.handleLocalError(msg.err)
	}
	if msg.cfg == nil || msg.keybindings == nil {
		m.runtimeRequest().finish(msg.requestID)
		return m.handleLocalError(errors.New("reload returned incomplete configuration"))
	}

	runtimeCfg, err := m.runtimeConfigForActivePreset(msg.cfg)
	if err != nil {
		m.runtimeRequest().finish(msg.requestID)
		return m.handleLocalError(fmt.Errorf("reload active preset: %w", err))
	}
	if m.Model.Switcher == nil && m.Model.Runner != nil {
		m.runtimeRequest().finish(msg.requestID)
		return m.handleLocalError(errors.New("reload runtime switcher is unavailable"))
	}

	transition := newRuntimeTransition(msg.cfg, runtimeCfg, m.activePreset(), "")
	if m.Model.Switcher == nil {
		m.runtimeRequest().finish(msg.requestID)
		var commitErr error
		m, commitErr = m.commitRuntimeTransition(transition)
		if commitErr != nil {
			return m, TransitionErrorCmd(commitErr)
		}
		m.Keybindings = msg.keybindings
		return m, m.terminalCommit().Entries(systemEntry("Configuration reloaded"))
	}

	return m.switchRuntimeCommandWithOptions(
		transition,
		systemEntry("Configuration reloaded"),
		m.currentMaterializedSessionID(),
		false,
		runtimeSwitchOptions{keybindings: msg.keybindings},
	)
}

func (m Model) showScopedModels() (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(err.Error())
	}
	resolved := resolveScopedModelPatterns(context.Background(), cfg)
	if len(resolved) == 0 {
		return m, m.terminalCommit().Entries(systemEntry("No scoped models configured"))
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.cardTopBorder("Scoped Models"))
	b.WriteString("\n")
	for i, sm := range resolved {
		line := fmt.Sprintf("  %d. %s", i+1, sm.Model)
		if sm.Provider != "" {
			line += fmt.Sprintf(" (provider: %s)", sm.Provider)
		}
		if sm.Thinking != "" {
			line += fmt.Sprintf(" [thinking: %s]", sm.Thinking)
		}
		b.WriteString(m.cardPaddedLine(m.st.dim, line))
		b.WriteString("\n")
	}
	b.WriteString(m.cardDivider())
	b.WriteString("\n")
	b.WriteString(m.cardPaddedLine(m.st.dim, "  Ctrl+L/Shift+Ctrl+L to cycle models"))
	b.WriteString("\n")
	b.WriteString(m.cardBottomBorder())
	return m, m.terminalCommit().Help(b.String())
}

func (m Model) handlePrimaryCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /primary")
	}
	return m.switchPresetCommand(PresetPrimary)
}

func (m Model) handleFastCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /fast")
	}
	return m.switchPresetCommand(PresetFast)
}

func (m Model) handleResumeCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) < 2 {
		return m.openSessionPicker()
	}
	return m.resumeStoredSessionByID(fields[1])
}

func (m Model) handleModelCommand(fields []string) (Model, tea.Cmd) {
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
	transition, runtimeCfg, err := m.modelSelectionTransition(cfg, m.activePreset(), name)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	if runtimeCfg.Provider == "" {
		return m, cmdError("cannot set model without an active provider; use /provider first")
	}
	return m.switchRuntimeCommand(transition,
		systemEntry("Model set to "+name),
		m.currentMaterializedSessionID(),
		false,
	)
}

func (m Model) handleThinkingCommand(fields []string) (Model, tea.Cmd) {
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
	transition, _, err := m.thinkingSelectionTransition(cfg, m.activePreset(), level)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	return m.beginRuntimeTransitionCommit(transition,
		systemEntry("Thinking set to "+thinkingDisplayName(level)),
	)
}

func (m Model) handleProviderCommandDispatch(fields []string) (Model, tea.Cmd) {
	if len(fields) < 2 {
		return m.openProviderSetupPicker()
	}
	return m.handleProviderCommand(fields[1])
}

func (m Model) handleLoginCommand(fields []string) (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	provider := cfg.Provider
	if len(fields) >= 2 {
		provider = fields[1]
	}
	if strings.TrimSpace(provider) == "" {
		return m.openProviderSetupPicker()
	}
	return m.openAPIKeyPrompt(cfgForProvider(cfg, provider), provider, m.activePreset())
}

func (m Model) handleToolsCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /tools")
	}
	summarizer, ok := m.Model.Info.(ToolSummarizer)
	if !ok {
		return m, cmdError("tool summary unavailable for this runtime")
	}
	return m, m.terminalCommit().Entries(
		systemEntry(toolSurfaceSummary(summarizer.ToolSurface())),
	)
}
