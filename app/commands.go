package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
	tea "charm.land/bubbletea/v2"
	ionclipboard "github.com/nijaru/ion/internal/clipboard"
	ionexport "github.com/nijaru/ion/internal/export"
	ionskills "github.com/nijaru/ion/internal/skills"
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
	if !commandInfo.Available {
		return m, cmdError(deferredFeatureMessage(commandInfo.Name))
	}
	if m.commandRequiresIdle(commandInfo, fields) && m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage(commandInfo.Name))
	}

	switch commandInfo.Name {
	case "/help":
		return m, m.terminalCommit().Help(helpText())

	case "/hotkeys":
		return m, m.terminalCommit().Help(hotkeysText())

	case "/reload":
		return m.reloadConfig()

	case "/scoped-models":
		return m.showScopedModels()

	case "/primary":
		if len(fields) != 1 {
			return m, cmdError("usage: /primary")
		}
		return m.switchPresetCommand(runtime.PresetPrimary)

	case "/fast":
		if len(fields) != 1 {
			return m, cmdError("usage: /fast")
		}
		return m.switchPresetCommand(runtime.PresetFast)

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
			return m.openProviderSetupPicker()
		}
		return m.handleProviderCommand(fields[1])

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
			return m.openProviderSetupPicker()
		}
		return m.openAPIKeyPrompt(cfgForProvider(cfg, provider), provider, m.activePreset())

	case "/logout":
		return m.logoutProvider()

	case "/settings":
		return m.handleSettingsCommand(fields)

	case "/tools":
		if len(fields) != 1 {
			return m, cmdError("usage: /tools")
		}
		summarizer, ok := m.Model.Backend.(ToolSummarizer)
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

	case "/changelog":
		if len(fields) != 1 {
			return m, cmdError("usage: /changelog")
		}
		content, err := os.ReadFile("CHANGELOG.md")
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to read CHANGELOG.md: %v", err))
		}
		return m, m.terminalCommit().Entries(systemEntry(string(content)))

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
		if m.Model.Storage != nil && !runtime.IsMaterialized(m.Model.Storage) {
			return m, m.terminalCommit().Entries(systemEntry("No active session to compact yet"))
		}
		compactor, ok := m.Model.Backend.(Compactor)
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

	case "/export-html":
		return m.exportSessionHTML()

	case "/import":
		if len(fields) < 2 {
			return m, cmdError("usage: /import <filename>")
		}
		return m.importSession(fields[1])

	case "/name":
		if len(fields) < 2 {
			return m, cmdError("usage: /name <session-name>")
		}
		return m.nameSession(strings.Join(fields[1:], " "))

	case "/clone":
		return m.cloneSession()

	case "/copy":
		return m.copyLastResponse()

	case "/debug":
		return m.handleDebugCommand()

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
		m.Picker.SetupSaveRequest != 0 ||
		m.Model.SettingsRequest != 0
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

func (m Model) showSessionTree() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	reader, ok := m.Model.Store.(runtime.SessionTreeReader)
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
	tree runtime.SessionTree
}

func (m Model) handleSessionTree(msg sessionTreeMsg) (Model, tea.Cmd) {
	tree := msg.tree
	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.cardTopBorder("Session Tree"))
	b.WriteString("\n")
	b.WriteString(m.cardPaddedLine(m.st.dim, "  Current: "+tree.Current.ID()))
	b.WriteString("\n")
	if session.EntryTitle(tree.Current) != "" {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Title: "+session.EntryTitle(tree.Current)))
		b.WriteString("\n")
	}
	if len(tree.Lineage) > 0 {
		b.WriteString(m.cardDivider())
		b.WriteString("\n")
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Lineage:"))
		b.WriteString("\n")
		for _, info := range tree.Lineage {
			title := info.ID()
			if session.EntryTitle(info) != "" {
				title = session.EntryTitle(info)
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
			title := info.ID()
			if session.EntryTitle(info) != "" {
				title = session.EntryTitle(info)
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
	exporter, ok := m.Model.Store.(runtime.SessionBundleExporter)
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

func (m Model) exportSessionHTML() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	exporter, ok := m.Model.Store.(runtime.SessionBundleExporter)
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
		htmlContent, err := ionexport.BundleToHTML(bundle)
		if err != nil {
			return localErrorMsg{err: err}
		}
		filename := fmt.Sprintf("session-%s.html", sessionID[:min(8, len(sessionID))])
		if err := os.WriteFile(filename, []byte(htmlContent), 0644); err != nil {
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
	importer, ok := m.Model.Store.(runtime.SessionBundleImporter)
	if !ok {
		return m, cmdError("store does not support import")
	}
	return m, func() tea.Msg {
		data, err := os.ReadFile(filename)
		if err != nil {
			return localErrorMsg{err: err}
		}
		var bundle runtime.SessionBundle
		if err := json.Unmarshal(data, &bundle); err != nil {
			return localErrorMsg{err: err}
		}
		imported, err := importer.ImportSessionBundle(context.Background(), bundle)
		if err != nil {
			return localErrorMsg{err: err}
		}
		return sessionImportedMsg{sessionID: imported, filename: filename}
	}
}

type sessionImportedMsg struct {
	sessionID string
	filename string
}

func (m Model) handleSessionImported(msg sessionImportedMsg) (Model, tea.Cmd) {
	notice := fmt.Sprintf("Imported %d session(s) from %s", 1, msg.filename)
	m.terminalCommit().Entries(systemEntry(notice))
	return m, nil
}

func (m Model) nameSession(name string) (Model, tea.Cmd) {
	if m.Model.Storage == nil {
		return m, cmdError("no active session")
	}
	storage := m.Model.Storage
	return m, func() tea.Msg {
		_, err := storage.AppendSessionInfo(context.Background(), name)
		if err != nil {
			return localErrorMsg{err: err}
		}
		return sessionNamedMsg{name: name}
	}
}

type sessionNamedMsg struct {
	name string
}

func (m Model) handleSessionNamed(msg sessionNamedMsg) (Model, tea.Cmd) {
	m.terminalCommit().Entries(systemEntry("Session named: " + msg.name))
	return m, nil
}

func (m Model) logoutProvider() (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	provider := cfg.Provider
	if provider == "" {
		return m, cmdError("no active provider")
	}
	if err := config.SaveAPIKey(provider, ""); err != nil {
		return m, cmdError(fmt.Sprintf("failed to clear API key: %v", err))
	}
	m.terminalCommit().Entries(systemEntry("Logged out from " + provider))
	return m, nil
}

func (m Model) copyLastResponse() (Model, tea.Cmd) {
	if m.Model.Storage == nil {
		return m, cmdError("no active session")
	}
	entries, err := m.Model.Storage.Entries(context.Background())
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to get entries: %v", err))
	}
	// Find the last assistant message
	var lastResponse string
	for i := len(entries) - 1; i >= 0; i-- {
		if session.EntryRole(entries[i]) == "assistant" {
			lastResponse = session.EntryText(entries[i])
			break
		}
	}
	if lastResponse == "" {
		return m, cmdError("no assistant response to copy")
	}
	if err := ionclipboard.WriteClipboardText(lastResponse); err != nil {
		return m, cmdError(fmt.Sprintf("failed to copy: %v", err))
	}
	m.terminalCommit().Entries(systemEntry("Copied last response to clipboard"))
	return m, nil
}

func (m Model) cloneSession() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	exporter, ok := m.Model.Store.(runtime.SessionBundleExporter)
	if !ok {
		return m, cmdError("store does not support session export")
	}
	importer, ok := m.Model.Store.(runtime.SessionBundleImporter)
	if !ok {
		return m, cmdError("store does not support session import")
	}
	if m.Model.Session == nil {
		return m, cmdError("no active session")
	}
	sessionID := m.Model.Session.ID()
	return m, func() tea.Msg {
		ctx := context.Background()
		bundle, err := exporter.ExportSessionBundle(ctx, sessionID)
		if err != nil {
			return localErrorMsg{err: fmt.Errorf("export session: %w", err)}
		}
		// Clear the root session ID so import creates a new session
		bundle.RootSessionID = ""
		imported, err := importer.ImportSessionBundle(ctx, bundle)
		if err != nil {
			return localErrorMsg{err: fmt.Errorf("import session: %w", err)}
		}
		if len(imported) == 0 {
			return localErrorMsg{err: fmt.Errorf("no sessions imported")}
		}
		return sessionClonedMsg{newSessionID: imported}
	}
}

type sessionClonedMsg struct {
	newSessionID string
}

func (m Model) handleSessionCloned(msg sessionClonedMsg) (Model, tea.Cmd) {
	return m, m.terminalCommit().Entries(systemEntry("Cloned session " + msg.newSessionID))
}

func (m Model) reloadConfig() (Model, tea.Cmd) {
	// Reload keybindings
	km, err := LoadKeybindings()
	if err != nil {
		return m, cmdError(fmt.Sprintf("reload keybindings: %v", err))
	}
	m.Keybindings = km

	// Reload model config
	if cfg, err := config.Load(); err == nil {
		m.Model.Config = cfg
	}

	return m, m.terminalCommit().Entries(systemEntry("Configuration reloaded"))
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

func helpText() string                                 { return HelpText() }
func hotkeysText() string                              { return HotkeysText() }
func slashCommands() []string                          { return SlashCommands() }
func deferredFeatureMessage(f string) string           { return DeferredFeatureMessage(f) }
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

func (m Model) handleSettingsCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) == 1 {
		return m.openSettingsPicker()
	}
	if len(fields) != 3 {
		return m, cmdError(
			"usage: /settings [retry on|off|tool auto|full|collapsed|hidden|tool_mode coding|read|all|read full|summary|hidden|write diff|summary|hidden|bash full|summary|hidden|thinking on|off|busy queue|steer]",
		)
	}

	key := strings.ToLower(strings.TrimSpace(fields[1]))
	value := strings.ToLower(strings.TrimSpace(fields[2]))
	if _, _, err := settingsConfigUpdate(&config.Config{}, key, value); err != nil {
		return m, cmdError(err.Error())
	}
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing settings"))
	}
	if m.Model.SettingsRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing settings"))
	}
	m.Model.SettingsRequest++
	requestID := m.Model.SettingsRequest
	m.progressReducer().beginLocalStatus("Saving settings...")
	activeCfg := m.Model.Config
	preset := m.activePreset()
	return m, func() tea.Msg {
		stableCfg, err := loadStableConfig()
		if err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to load config: %w", err),
			}
		}
		updated, notice, err := settingsConfigUpdate(stableCfg, key, value)
		if err != nil {
			return settingsCommandMsg{requestID: requestID, err: err}
		}
		if err := saveConfigFile(&updated); err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to save config: %w", err),
			}
		}
		runtimeCfg, err := loadConfigFile()
		if err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to reload runtime config: %w", err),
			}
		}
		mergeRuntimeSelection(runtimeCfg, activeCfg)
		backendCfg, err := m.runtimeConfigForPreset(runtimeCfg, preset)
		if err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to resolve active preset: %w", err),
			}
		}
		return settingsCommandMsg{
			requestID:     requestID,
			transition:    newRuntimeTransition(runtimeCfg, backendCfg, preset, ""),
			hasTransition: true,
			notice:        notice,
		}
	}
}

func (m Model) handleSettingsCommandResult(msg settingsCommandMsg) (Model, tea.Cmd) {
	if msg.requestID == 0 || msg.requestID != m.Model.SettingsRequest {
		return m, nil
	}
	m.Model.SettingsRequest = 0
	m.progressReducer().clearLocalBusyStatus()
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	if !msg.hasTransition {
		return m, nil
	}
	var err error
	m, err = m.commitRuntimeTransition(msg.transition)
	if err != nil {
		return m, TransitionErrorCmd(err)
	}
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeSettings {
		items := settingsPickerItems(m.Model.Config)
		m.Picker.Overlay.items = items
		m.Picker.Overlay.cfg = m.Model.Config
		m.pickerReducer().refreshOverlayFilter()
		return m, nil
	}
	return m, m.terminalCommit().Entries(systemEntry(msg.notice))
}

func settingsConfigUpdate(
	cfg *config.Config,
	key string,
	value string,
) (config.Config, string, error) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	var notice string

	switch key {
	case "retry":
		enabled, ok := parseOnOff(value)
		if !ok {
			return config.Config{}, "", fmt.Errorf("usage: /settings retry on|off")
		}
		updated.RetryUntilCancelled = &enabled
		if enabled {
			notice = "Retry network errors: on"
		} else {
			notice = "Retry network errors: off"
		}
	case "tool", "tools":
		verbosity, ok := parseToolVerbosity(value)
		if !ok {
			return config.Config{}, "", fmt.Errorf(
				"usage: /settings tool auto|full|collapsed|hidden",
			)
		}
		updated.ToolVerbosity = verbosity
		notice = "Tool display: " + displayToolVerbosity(verbosity)
	case "tool_mode", "toolmode", "active_tools":
		mode := config.NormalizeToolMode(value)
		if mode == "coding" && value != "coding" {
			return config.Config{}, "", fmt.Errorf("usage: /settings tool_mode coding|read|all")
		}
		if mode == "coding" {
			updated.ToolMode = ""
		} else {
			updated.ToolMode = mode
		}
		notice = "Tool mode: " + mode
	case "read":
		output := config.NormalizeReadOutput(value)
		if output == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings read full|summary|hidden")
		}
		updated.ReadOutput = output
		notice = "Read output: " + displayReadOutput(output)
	case "write":
		output := config.NormalizeWriteOutput(value)
		if output == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings write diff|summary|hidden")
		}
		updated.WriteOutput = output
		notice = "Write output: " + displayWriteOutput(output)
	case "bash":
		output := config.NormalizeBashOutput(value)
		if output == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings bash full|summary|hidden")
		}
		updated.BashOutput = output
		notice = "Bash output: " + displayBashOutput(output)
	case "thinking":
		if value == "" {
			return config.Config{}, "", fmt.Errorf(
				"usage: /settings thinking on|off",
			)
		}
		switch strings.ToLower(value) {
		case "on", "full", "show":
			updated.ThinkingVerbosity = "full"
			notice = "Thinking content: visible"
		case "off", "hidden", "collapse", "collapsed":
			updated.ThinkingVerbosity = ""
			notice = "Thinking content: hidden"
		default:
			return config.Config{}, "", fmt.Errorf(
				"usage: /settings thinking on|off",
			)
		}
	case "reasoning", "reasoning_effort", "thinking_level":
		level := config.NormalizeReasoningEffort(value)
		if level == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings thinking_level auto|off|minimal|low|medium|high|xhigh|max")
		}
		updated.ReasoningEffort = level
		notice = "Thinking level: " + level
	case "busy", "busy_input":
		mode := config.NormalizeBusyInput(value)
		if mode == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings busy queue|steer")
		}
		if mode == "queue" {
			updated.BusyInput = "queue"
		} else {
			updated.BusyInput = ""
		}
		notice = "Busy input: " + mode
	default:
		return config.Config{}, "", fmt.Errorf(
			"usage: /settings [retry|tool|tool_mode|read|write|bash|thinking|busy|reasoning] ...",
		)
	}
	return updated, notice, nil
}

func (m Model) openSettingsPicker() (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("opening settings"))
	}
	if m.Model.SettingsRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("opening settings"))
	}
	cfg := &config.Config{}
	if m.Model.Config != nil {
		clone := *m.Model.Config
		cfg = &clone
	}
	items := settingsPickerItems(cfg)
	m.clearProgressError()
	m.pickerReducer().openOverlay(pickerOverlayState{
		title:    "Settings",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    0,
		purpose:  pickerPurposeSettings,
		cfg:      cfg,
	})
	return m, nil
}

func settingsPickerItems(cfg *config.Config) []pickerItem {
	if cfg == nil {
		cfg = &config.Config{}
	}
	retry := onOff(cfg.RetryUntilCancelledEnabled())
	busy := cfg.BusyInputMode()
	toolDisplay := displayToolVerbosity(cfg.ToolVerbosity)
	thinkingOutput := displayThinkingVerbosity(cfg.ThinkingVerbosity)
	reasoning := displayReasoningEffort(cfg.ReasoningEffort)
	toolMode := displayToolMode(cfg.ToolMode)

	return []pickerItem{
		settingsPickerItem(
			"Retry network errors",
			"retry",
			retry,
			toggleOnOff(retry),
			"Turn behavior",
			"Retry transient provider/network failures",
		),
		settingsPickerItem(
			"Active turn input",
			"busy",
			busy,
			toggleBusyInput(busy),
			"Turn behavior",
			"Default running-turn input behavior",
		),
		settingsPickerItem(
			"Thinking level",
			"reasoning",
			reasoning,
			nextSettingValue(reasoning, []string{"auto", "off", "low", "medium", "high"}),
			"Turn behavior",
			"Reasoning depth for thinking models",
		),
		settingsPickerItem(
			"Tool permission",
			"tool_mode",
			toolMode,
			nextSettingValue(toolMode, []string{"coding", "read", "all"}),
			"Turn behavior",
			"Execution rights for agent tools",
		),
		settingsPickerItem(
			"Tool display",
			"tool",
			toolDisplay,
			nextSettingValue(toolDisplay, []string{"collapsed", "full", "hidden"}),
			"Display",
			"Tool call/result visibility",
		),
		settingsPickerItem(
			"Thinking content",
			"thinking",
			thinkingOutput,
			nextSettingValue(thinkingOutput, []string{"off", "on"}),
			"Display",
			"Reasoning transcript detail",
		),
	}
}

func settingsPickerItem(label, key, current, next, group, detail string) pickerItem {
	itemLabel := label + ": " + current
	itemDetail := "Enter: " + current + " -> " + next
	if detail != "" {
		itemDetail += " • " + detail
	}
	return pickerItem{
		Label:       itemLabel,
		Value:       key + " " + next,
		Detail:      itemDetail,
		Group:       group,
		SettingName: label,
		CurrentVal:  current,
		Desc:        detail,
		Search: pickerSearchIndex(
			itemLabel,
			key+" "+current+" "+next,
			detail,
			group,
			nil,
		),
	}
}

func toggleOnOff(value string) string {
	if value == "on" {
		return "off"
	}
	return "on"
}

func toggleBusyInput(value string) string {
	if value == "queue" {
		return "steer"
	}
	return "queue"
}

func nextSettingValue(current string, values []string) string {
	if len(values) == 0 {
		return current
	}
	for i, value := range values {
		if value == current {
			return values[(i+1)%len(values)]
		}
	}
	return values[0]
}

func parseOnOff(value string) (bool, bool) {
	switch value {
	case "on", "true", "yes":
		return true, true
	case "off", "false", "no":
		return false, true
	default:
		return false, false
	}
}

func onOff(enabled bool) string {
	if enabled {
		return "on"
	}
	return "off"
}

func parseToolVerbosity(value string) (string, bool) {
	if strings.EqualFold(strings.TrimSpace(value), "auto") {
		return "", true
	}
	normalized := config.NormalizeVerbosity(value)
	return normalized, normalized != ""
}

func displayToolVerbosity(value string) string {
	if normalized := config.NormalizeVerbosity(value); normalized != "" {
		return normalized
	}
	return "collapsed"
}

func displayReadOutput(value string) string {
	if normalized := config.NormalizeReadOutput(value); normalized != "" {
		return normalized
	}
	return "summary"
}

func displayWriteOutput(value string) string {
	if normalized := config.NormalizeWriteOutput(value); normalized != "" {
		return normalized
	}
	return "summary"
}

func displayBashOutput(value string) string {
	if normalized := config.NormalizeBashOutput(value); normalized != "" {
		return normalized
	}
	return "hidden"
}

func displayThinkingVerbosity(value string) string {
	if value == "full" {
		return "on"
	}
	return "off"
}

func displayReasoningEffort(value string) string {
	if normalized := config.NormalizeReasoningEffort(value); normalized != "" {
		return normalized
	}
	return "auto"
}

func displayToolMode(value string) string {
	if normalized := config.NormalizeToolMode(value); normalized != "" {
		return normalized
	}
	return "coding"
}

func (m Model) costBudgetNotice(inputTokens, outputTokens int, totalCost float64) string {
	totalTokens := inputTokens + outputTokens
	lines := []string{
		"Session cost",
		fmt.Sprintf("input tokens: %d", inputTokens),
		fmt.Sprintf("output tokens: %d", outputTokens),
		fmt.Sprintf("total tokens: %d", totalTokens),
		fmt.Sprintf("cost: $%.6f", totalCost),
	}
	if m.Model.Config != nil && m.Model.Config.MaxSessionCost > 0 {
		lines = append(lines, fmt.Sprintf("session limit: $%.6f", m.Model.Config.MaxSessionCost))
		remaining := m.Model.Config.MaxSessionCost - totalCost
		if remaining < 0 {
			remaining = 0
		}
		lines = append(lines, fmt.Sprintf("session remaining: $%.6f", remaining))
	}
	if m.Model.Config != nil && m.Model.Config.MaxTurnCost > 0 {
		lines = append(lines, fmt.Sprintf("turn limit: $%.6f", m.Model.Config.MaxTurnCost))
	}
	return strings.Join(lines, "\n")
}

func (m Model) handleSessionCompacted(msg sessionCompactedMsg) (Model, tea.Cmd) {
	m.progressReducer().completeCompaction()
	cmds := []tea.Cmd{m.terminalCommit().Entries(systemEntry(msg.notice))}
	if queued := m.turnReducer().PopQueuedTurn(); queued != "" {
		cmds = append(cmds, func() tea.Msg {
			return queuedTurnMsg{text: queued, rearmSessionEvents: false}
		})
	}
	return m, tea.Sequence(cmds...)
}

func (m Model) handleSessionCost(msg sessionCostMsg) (Model, tea.Cmd) {
	return m, m.terminalCommit().Entries(systemEntry(msg.notice))
}

func loadSessionUsageCmd(generation uint64, sess session.Session) tea.Cmd {
	if sess == nil {
		return nil
	}
	return func() tea.Msg {
		usage, err := sess.Usage(context.Background())
		return sessionUsageLoadedMsg{
			generation: generation,
			input:      usage.Input,
			output:     usage.Output,
			cost:       usage.Cost.Total,
			err:        err,
		}
	}
}

func (m Model) handleSessionUsageLoaded(msg sessionUsageLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.err != nil {
		return m, nil
	}
	m.progressReducer().applySessionUsage(msg.input, msg.output, msg.cost)
	return m, nil
}

func (m Model) sessionCostCmd() tea.Cmd {
	return func() tea.Msg {
		inputTokens := m.Progress.TokensSent
		outputTokens := m.Progress.TokensReceived
		totalCost := m.Progress.TotalCost
		if m.Model.Storage != nil {
			usage, err := m.Model.Storage.Usage(context.Background())
			if err != nil {
				return localErrorMsg{err: fmt.Errorf("failed to load session usage: %w", err)}
			}
			inputTokens = usage.Input
			outputTokens = usage.Output
			totalCost = usage.Cost.Total
		}
		if totalCost <= 0 {
			if m.Model.Config != nil &&
				(m.Model.Config.MaxSessionCost > 0 || m.Model.Config.MaxTurnCost > 0) {
				return sessionCostMsg{
					notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost),
				}
			}
			return sessionCostMsg{notice: "No API cost tracked for this session"}
		}
		return sessionCostMsg{notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost)}
	}
}

func (m Model) sessionInfoCmd() tea.Cmd {
	return func() tea.Msg {
		notice, err := m.sessionInfoNotice()
		if err != nil {
			return localErrorMsg{err: err}
		}
		return localEntriesMsg{
			entries: []session.Entry{systemEntry(notice)},
		}
	}
}

func (m Model) sessionInfoNotice() (string, error) {
	sessionID := ""
	if m.Model.Runtime.Materialized {
		sessionID = m.Model.Runtime.SessionID
	}
	if m.Model.Storage != nil {
		if sessionID == "" && runtime.IsMaterialized(m.Model.Storage) {
			sessionID = strings.TrimSpace(m.Model.Storage.ID())
		}
	} else if m.Model.Session != nil {
		sessionID = strings.TrimSpace(m.Model.Session.ID())
	}
	if sessionID == "" {
		sessionID = "none"
	}

	provider := m.runtimeProvider()
	model := m.runtimeModel()
	if provider == "" {
		provider = "unknown"
	}
	if model == "" {
		model = "unknown"
	}

	inputTokens, outputTokens, totalCost := m.Progress.TokensSent, m.Progress.TokensReceived, m.Progress.TotalCost
	var entries []session.Entry
	if m.Model.Storage != nil {
		usage, err := m.Model.Storage.Usage(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session usage: %w", err)
		}
		inputTokens = usage.Input
		outputTokens = usage.Output
		totalCost = usage.Cost.Total
		loaded, err := m.Model.Storage.Entries(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session entries: %w", err)
		}
		entries = loaded
	}

	counts := sessionEntryCounts(entries)
	lines := []string{
		"Session",
		"id: " + sessionID,
		"provider: " + provider,
		"model: " + model,
	}
	if branch := strings.TrimSpace(m.App.Branch); branch != "" {
		lines = append(lines, "branch: "+branch)
	}
	lines = append(
		lines,
		fmt.Sprintf("messages: user %d, assistant %d, tools %d, total %d",
			counts.user, counts.agent, counts.tool, counts.total),
		fmt.Sprintf("tokens: input %d, output %d, total %d",
			inputTokens, outputTokens, inputTokens+outputTokens),
		fmt.Sprintf("cost: $%.6f", totalCost),
	)
	return strings.Join(lines, "\n"), nil
}

type sessionCounts struct {
	user  int
	agent int
	tool  int
	total int
}

func sessionEntryCounts(entries []session.Entry) sessionCounts {
	var counts sessionCounts
	for _, entry := range entries {
		counts.total++
		switch session.EntryRole(entry) {
		case "user":
			counts.user++
		case "assistant":
			counts.agent++
		case "tool_result":
			counts.tool++
		}
	}
	return counts
}
