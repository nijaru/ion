package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
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
		return m, func() tea.Msg {
			return sessionHelpMsg{notice: helpText()}
		}

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
		return m, m.resumeStoredSessionByID(fields[1])
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
		if err := config.SaveState(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Model.Backend.SetConfig(runtimeCfg)
		if runtimeCfg.Provider == "" {
			m.Progress.Status = noProviderConfiguredStatus()
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Model set to " + name},
			)
		}
		return m, m.switchRuntimeCommand(
			runtimeCfg,
			updated,
			m.activePreset(),
			session.Entry{Role: session.System, Content: "Model set to " + name},
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
		updated := m.updateThinkingForActivePreset(cfg, level)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.SaveReasoningState(m.activePreset().String(), level); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Model.Backend.SetConfig(runtimeCfg)
		m.Model.Config = updated
		m.Progress.ReasoningEffort = level
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
		updated := m.updateProviderForActivePreset(cfg, name)
		if err := config.SaveState(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Model.Backend.SetConfig(updated)
		m.Model.Config = updated
		m.clearProgressError()
		m.Progress.Status = noModelConfiguredStatus()
		return m.openModelPickerWithConfig(updated)

	case "/settings":
		return m.handleSettingsCommand(fields)

	case "/mcp":
		if len(fields) < 3 || fields[1] != "add" {
			return m, cmdError("usage: /mcp add <command> [args...]")
		}
		mcpCmd := fields[2]
		mcpArgs := fields[3:]
		sess := m.Model.Session
		return m, func() tea.Msg {
			if err := sess.RegisterMCPServer(context.Background(), mcpCmd, mcpArgs...); err != nil {
				return localErrorMsg{err: err}
			}
			return nil
		}

	case "/read":
		return m.setModeCommand(session.ModeRead)

	case "/edit":
		return m.setModeCommand(session.ModeEdit)

	case "/auto", "/yolo":
		return m.setModeCommand(session.ModeYolo)

	case "/mode":
		if len(fields) < 2 {
			modeName := modeDisplayName(m.Mode)
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Current mode: " + modeName},
			)
		}
		switch strings.ToLower(fields[1]) {
		case "read", "r":
			return m.setModeCommand(session.ModeRead)
		case "edit", "e", "write", "w":
			return m.setModeCommand(session.ModeEdit)
		case "auto", "a", "yolo", "y":
			return m.setModeCommand(session.ModeYolo)
		default:
			return m, cmdError("usage: /mode [read|edit|auto]")
		}

	case "/trust":
		if len(fields) > 1 && fields[1] != "status" {
			return m, cmdError("usage: /trust [status]")
		}
		if len(fields) > 1 && fields[1] == "status" {
			status := "not trusted"
			if m.App.TrustedWorkspace {
				status = "trusted"
			}
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Workspace trust: " + status},
			)
		}
		if m.Model.TrustStore == nil {
			return m, cmdError("workspace trust store is unavailable")
		}
		if m.App.WorkspaceTrust == "strict" {
			return m, cmdError(
				"workspace trust is strict; trust must be managed outside this session",
			)
		}
		if err := m.Model.TrustStore.Trust(m.App.Workdir); err != nil {
			return m, cmdError(fmt.Sprintf("failed to trust workspace: %v", err))
		}
		m.App.TrustedWorkspace = true
		m.Mode = session.ModeEdit
		m.Model.Session.SetMode(m.Mode)
		m.Model.Session.SetAutoApprove(false)
		return m, m.printEntries(
			session.Entry{Role: session.System, Content: "Workspace trusted. Mode: EDIT"},
		)

	case "/rewind":
		if len(fields) < 2 || len(fields) > 3 {
			return m, cmdError("usage: /rewind <checkpoint-id> [--confirm]")
		}
		confirmed := len(fields) == 3 && fields[2] == "--confirm"
		if len(fields) == 3 && !confirmed {
			return m, cmdError("usage: /rewind <checkpoint-id> [--confirm]")
		}
		return m.rewindCheckpointCommand(fields[1], confirmed)

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

	case "/fork":
		if m.Model.Storage == nil || !storage.IsMaterialized(m.Model.Storage) {
			return m, cmdError("No active session to fork yet")
		}
		parentID := m.currentMaterializedSessionID()
		if parentID == "" {
			return m, cmdError("No active session to fork yet")
		}
		forker, ok := m.Model.Store.(storage.SessionForker)
		if !ok {
			return m, cmdError("session store does not support forking")
		}
		label := strings.TrimSpace(strings.TrimPrefix(input, command))
		forked, err := forker.ForkSession(context.Background(), parentID, storage.ForkOptions{
			Label:  label,
			Reason: "user requested /fork",
		})
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to fork session: %v", err))
		}
		defer func() {
			_ = forked.Close()
		}()
		meta := forked.Meta()
		provider, model := splitStoredSessionModel(meta.Model)
		if provider == "" || model == "" {
			return m, cmdError(
				fmt.Sprintf("forked session %s is missing provider/model metadata", forked.ID()),
			)
		}
		cfg := &config.Config{Provider: provider, Model: model}
		notice := session.Entry{Role: session.System, Content: "Forked session " + forked.ID()}
		return m, m.resumeRuntimeCommand(cfg, notice, forked.ID())

	case "/tree":
		if len(fields) != 1 {
			return m, cmdError("usage: /tree")
		}
		if m.Model.Storage == nil || !storage.IsMaterialized(m.Model.Storage) {
			return m, cmdError("No active session tree yet")
		}
		sessionID := m.currentMaterializedSessionID()
		if sessionID == "" {
			return m, cmdError("No active session tree yet")
		}
		reader, ok := m.Model.Store.(storage.SessionTreeReader)
		if !ok {
			return m, cmdError("session store does not support tree view")
		}
		tree, err := reader.SessionTree(context.Background(), sessionID)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load session tree: %v", err))
		}
		return m, m.printEntries(session.Entry{
			Role:    session.System,
			Content: sessionTreeNotice(tree),
		})

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
		return m, m.switchRuntimeCommand(
			runtimeCfg,
			cfg,
			m.activePreset(),
			session.Entry{Role: session.System, Content: notice},
			"",
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
	return m.InFlight.Thinking || m.Progress.Compacting || m.Approval.Pending != nil
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

func (m Model) setModeCommand(mode session.Mode) (Model, tea.Cmd) {
	if m.trustGateActive() && !m.App.TrustedWorkspace && mode != session.ModeRead {
		return m, cmdError("Trust this workspace first with /trust.")
	}
	m.Mode = mode
	m.Model.Session.SetMode(m.Mode)
	m.Model.Session.SetAutoApprove(m.Mode == session.ModeYolo)
	notice := "Mode: " + modeDisplayName(m.Mode)
	if m.Mode == session.ModeYolo {
		if summarizer, ok := m.Model.Backend.(backend.ToolSummarizer); ok {
			if sandbox := strings.TrimSpace(summarizer.ToolSurface().Sandbox); sandbox != "" {
				notice += "\nSandbox: " + sandbox
			}
		}
	}
	return m, m.printEntries(session.Entry{Role: session.System, Content: notice})
}

func (m Model) trustGateActive() bool {
	return m.Model.TrustStore != nil && config.ResolveWorkspaceTrust(m.App.WorkspaceTrust) != "off"
}

// cmdError returns a Cmd that emits a local UI error with the given message.
func cmdError(msg string) tea.Cmd {
	return func() tea.Msg {
		return localErrorMsg{err: fmt.Errorf("%s", msg)}
	}
}

func modeDisplayName(mode session.Mode) string {
	switch mode {
	case session.ModeRead:
		return "READ"
	case session.ModeEdit:
		return "EDIT"
	case session.ModeYolo:
		return "AUTO"
	default:
		return "EDIT"
	}
}
