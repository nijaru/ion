package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/session"
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
		return m, cmdError("Finish or cancel the current turn before " + command + ".")
	}

	switch command {
	case "/help":
		return m, m.printHelp(helpText())

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
		currentCfg, err := m.runtimeConfigForActivePreset(cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" &&
			strings.EqualFold(strings.TrimSpace(currentCfg.Model), strings.TrimSpace(name)) {
			return m, nil
		}
		updated := m.updateModelForActivePreset(cfg, name)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if runtimeCfg.Provider == "" {
			transition := newRuntimeTransition(
				updated,
				runtimeCfg,
				m.activePreset(),
				noProviderConfiguredStatus(),
			).withStatePersistence()
			var commitErr error
			m, commitErr = m.commitRuntimeTransition(transition)
			if commitErr != nil {
				return m, runtimeTransitionErrorCmd(commitErr)
			}
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Model set to " + name},
			)
		}
		return m.switchRuntimeCommand(
			runtimeCfg,
			updated,
			m.activePreset(),
			session.Entry{Role: session.System, Content: "Model set to " + name},
			m.currentMaterializedSessionID(),
			false,
			true,
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
		updated := m.updateThinkingForActivePreset(cfg, level)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		transition := newRuntimeTransition(
			updated,
			runtimeCfg,
			m.activePreset(),
			"",
		).withReasoningPersistence(m.activePreset(), level)
		var commitErr error
		m, commitErr = m.commitRuntimeTransition(transition)
		if commitErr != nil {
			return m, runtimeTransitionErrorCmd(commitErr)
		}
		return m, m.printEntries(
			session.Entry{
				Role:    session.System,
				Content: "Thinking set to " + thinkingDisplayName(level),
			},
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
		updated, err := updateProviderSelection(cfg, name)
		if err != nil {
			return m, cmdError(err.Error())
		}
		if err := ensureProviderReadyForSelection(context.Background(), updated); err != nil {
			return m, cmdError(err.Error())
		}
		m.clearProgressError()
		if !providers.SupportsModelListing(updated) {
			transition := newRuntimeTransition(
				updated,
				updated,
				m.activePreset(),
				noModelConfiguredStatus(),
			).withStatePersistence()
			var commitErr error
			m, commitErr = m.commitRuntimeTransition(transition)
			if commitErr != nil {
				return m, runtimeTransitionErrorCmd(commitErr)
			}
			return m, m.printEntries(session.Entry{
				Role:    session.System,
				Content: providerModelEntryNotice(updated.Provider),
			})
		}
		return m.openModelPickerWithConfig(updated)

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
		return m, m.printEntries(
			session.Entry{Role: session.System, Content: toolSurfaceSummary(surface)},
		)

	case "/status":
		if len(fields) != 1 {
			return m, cmdError("usage: /status")
		}
		return m, m.printEntries(
			session.Entry{Role: session.System, Content: runtimeStatusSummary(m)},
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
		return m, m.printEntries(session.Entry{Role: session.System, Content: out})

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
			runtimeCfg.Provider = m.Model.Backend.Provider()
		}
		if runtimeCfg.Model == "" {
			runtimeCfg.Model = m.Model.Backend.Model()
		}
		if runtimeCfg.Provider == "" || runtimeCfg.Model == "" {
			return m, cmdError("cannot " + command + " without an active provider and model")
		}
		notice := "Started new session"
		if command == "/clear" {
			notice = "Started fresh session"
		}
		return m.switchRuntimeCommand(
			runtimeCfg,
			cfg,
			m.activePreset(),
			session.Entry{Role: session.System, Content: notice},
			"",
			false,
			false,
		)

	case "/cost":
		inputTokens, outputTokens, totalCost := m.Progress.TokensSent, m.Progress.TokensReceived, m.Progress.TotalCost
		if m.Model.Storage != nil {
			input, output, cost, err := m.Model.Storage.Usage(context.Background())
			if err != nil {
				return m, cmdError(fmt.Sprintf("failed to load session usage: %v", err))
			}
			inputTokens = input
			outputTokens = output
			totalCost = cost
		}
		if totalCost <= 0 {
			if m.Model.Config != nil &&
				(m.Model.Config.MaxSessionCost > 0 || m.Model.Config.MaxTurnCost > 0) {
				return m, func() tea.Msg {
					return sessionCostMsg{
						notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost),
					}
				}
			}
			return m, func() tea.Msg {
				return sessionCostMsg{notice: "No API cost tracked for this session"}
			}
		}
		return m, func() tea.Msg {
			return sessionCostMsg{notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost)}
		}

	case "/session":
		notice, err := m.sessionInfoNotice()
		if err != nil {
			return m, cmdError(err.Error())
		}
		return m, m.printEntries(session.Entry{Role: session.System, Content: notice})

	case "/compact":
		if m.Model.Storage != nil && !storage.IsMaterialized(m.Model.Storage) {
			return m, m.printEntries(session.Entry{
				Role:    session.System,
				Content: "No active session to compact yet",
			})
		}
		compactor, ok := m.Model.Backend.(backend.Compactor)
		if !ok {
			return m, cmdError("current backend does not support /compact")
		}
		m.Progress.Compacting = true
		m.Progress.Status = "Compacting context..."
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

func (m Model) localCommandBusy() bool {
	return m.InFlight.Thinking || m.Progress.Compacting
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
