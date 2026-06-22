package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	tea "charm.land/bubbletea/v2"
	ionskills "github.com/nijaru/ion/internal/skills"
	"github.com/nijaru/ion/internal/core"
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
		dir, err := config.DefaultSkillsDir()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve skills dir: %v", err))
		}
		detail, err := ionskills.Read([]string{dir}, name)
		if err != nil {
			return m, cmdError(err.Error())
		}
		return m, m.terminalCommit().Help(ionskills.FormatDetail(detail))
	}

	commandInfo, ok := resolveSlashCommand(command)
	if !ok {
		return m, cmdError(fmt.Sprintf("unknown command: %s", command))
	}
	if !commandInfo.Available() {
		return m, cmdError(deferredFeatureMessage(commandInfo.Name))
	}
	if m.commandRequiresIdle(commandInfo, fields) && m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage(commandInfo.Name))
	}

	switch commandInfo.Name {
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
		summarizer, ok := m.Model.Backend.(core.ToolSummarizer)
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
		if m.Model.Storage != nil && !session.IsMaterialized(m.Model.Storage) {
			return m, m.terminalCommit().Entries(systemEntry("No active session to compact yet"))
		}
		compactor, ok := m.Model.Backend.(core.Compactor)
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

	case "/tree":
		return m.showSessionTree()

	case "/export":
		return m.exportSession()

	case "/import":
		if len(fields) < 2 {
			return m, cmdError("usage: /import <filename>")
		}
		return m.importSession(fields[1])

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

	def, ok := llm.Lookup(provider)
	if !ok || def.Runtime != llm.RuntimeNative {
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

func (m Model) commandRequiresIdle(command core.SlashCommandInfo, fields []string) bool {
	switch command.Idle {
	case core.SlashCommandIdleAlways:
		return true
	case core.SlashCommandIdleWithArgs:
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

func (m Model) showSessionTree() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	reader, ok := m.Model.Store.(session.SessionTreeReader)
	if !ok {
		return m, cmdError("store does not support session tree")
	}
	if m.Model.Session == nil {
		return m, cmdError("no active session")
	}
	sessionID := m.Model.Session.ID()
	return m, func() tea.Msg {
		tree, err := reader.SessionTree(context.Background(), sessionID)
		if err != nil {
			return localErrorMsg{err: err}
		}
		return sessionTreeMsg{tree: tree}
	}
}

type sessionTreeMsg struct {
	tree session.SessionTree
}

func (m Model) handleSessionTree(msg sessionTreeMsg) (Model, tea.Cmd) {
	tree := msg.tree
	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.cardTopBorder("Session Tree"))
	b.WriteString("\n")
	b.WriteString(m.cardPaddedLine(m.st.dim, "  Current: "+tree.Current.ID))
	b.WriteString("\n")
	if tree.Current.Title != "" {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Title: "+tree.Current.Title))
		b.WriteString("\n")
	}
	if len(tree.Lineage) > 0 {
		b.WriteString(m.cardDivider())
		b.WriteString("\n")
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Lineage:"))
		b.WriteString("\n")
		for _, info := range tree.Lineage {
			title := info.ID
			if info.Title != "" {
				title = info.Title
			}
			b.WriteString(m.cardPaddedLine(m.st.dim, "    • "+title))
			b.WriteString("\n")
		}
	}
	if len(tree.Children) > 0 {
		b.WriteString(m.cardDivider())
		b.WriteString("\n")
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Children:"))
		b.WriteString("\n")
		for _, info := range tree.Children {
			title := info.ID
			if info.Title != "" {
				title = info.Title
			}
			b.WriteString(m.cardPaddedLine(m.st.dim, "    • "+title))
			b.WriteString("\n")
		}
	}
	b.WriteString(m.cardBottomBorder())
	m.terminalCommit().Entries(systemEntry(b.String()))
	return m, nil
}

func (m Model) exportSession() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	exporter, ok := m.Model.Store.(session.SessionBundleExporter)
	if !ok {
		return m, cmdError("store does not support export")
	}
	if m.Model.Session == nil {
		return m, cmdError("no active session")
	}
	sessionID := m.Model.Session.ID()
	return m, func() tea.Msg {
		bundle, err := exporter.ExportSessionBundle(context.Background(), sessionID)
		if err != nil {
			return localErrorMsg{err: err}
		}
		data, err := json.Marshal(bundle)
		if err != nil {
			return localErrorMsg{err: err}
		}
		filename := fmt.Sprintf("session-%s.json", sessionID)
		if err := os.WriteFile(filename, data, 0644); err != nil {
			return localErrorMsg{err: err}
		}
		return sessionExportedMsg{filename: filename}
	}
}

type sessionExportedMsg struct {
	filename string
}

func (m Model) handleSessionExported(msg sessionExportedMsg) (Model, tea.Cmd) {
	m.terminalCommit().Entries(systemEntry("Exported session to " + msg.filename))
	return m, nil
}

func (m Model) importSession(filename string) (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	importer, ok := m.Model.Store.(session.SessionBundleImporter)
	if !ok {
		return m, cmdError("store does not support import")
	}
	return m, func() tea.Msg {
		data, err := os.ReadFile(filename)
		if err != nil {
			return localErrorMsg{err: err}
		}
		var bundle session.SessionBundle
		if err := json.Unmarshal(data, &bundle); err != nil {
			return localErrorMsg{err: err}
		}
		imported, err := importer.ImportSessionBundle(context.Background(), bundle)
		if err != nil {
			return localErrorMsg{err: err}
		}
		return sessionImportedMsg{sessions: imported, filename: filename}
	}
}

type sessionImportedMsg struct {
	sessions []session.SessionInfo
	filename string
}

func (m Model) handleSessionImported(msg sessionImportedMsg) (Model, tea.Cmd) {
	notice := fmt.Sprintf("Imported %d session(s) from %s", len(msg.sessions), msg.filename)
	m.terminalCommit().Entries(systemEntry(notice))
	return m, nil
}
