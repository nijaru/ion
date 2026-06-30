package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
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
	case "/status":
		return m.handleStatusCommand(fields)
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
	case "/export":
		return m.exportSession()
	case "/export-html":
		return m.exportSessionHTML()
	case "/import":
		return m.handleImportCommand(fields)
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
type externalEditorFinishedMsg struct {
	content string
	err     error
}

var (
	externalEditorName            = externalEditor
	writeExternalEditorBufferFile = writeExternalEditorBuffer
)

func (m Model) openExternalEditor() (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, m.terminalCommit().Entries(
			systemEntry(m.localCommandBusyMessage("opening the external editor")),
		)
	}
	return m, openExternalEditorCmd(m.expandMarkers(m.Input.Composer.Value()))
}

func openExternalEditorCmd(content string) tea.Cmd {
	return func() tea.Msg {
		path, err := writeExternalEditorBufferFile(content)
		if err != nil {
			return externalEditorFinishedMsg{err: err}
		}

		editor := externalEditorName()
		cmd := externalEditorCommand(editor, path)
		return tea.ExecProcess(cmd, func(runErr error) tea.Msg {
			defer os.Remove(path)
			if runErr != nil {
				return externalEditorFinishedMsg{
					err: fmt.Errorf("%s failed: %w", editor, runErr),
				}
			}
			data, err := os.ReadFile(path)
			if err != nil {
				return externalEditorFinishedMsg{
					err: fmt.Errorf("read editor buffer: %w", err),
				}
			}
			return externalEditorFinishedMsg{content: string(data)}
		})()
	}
}

func (m Model) handleExternalEditorFinished(msg externalEditorFinishedMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		return m, m.terminalCommit().Entries(systemEntry("Editor failed: " + msg.err.Error()))
	}
	cmd := m.setComposerDraft(msg.content)
	m.resetHistoryCursor()
	m.clearPasteMarkers()
	return m, cmd
}

func externalEditor() string {
	if editor := strings.TrimSpace(os.Getenv("VISUAL")); editor != "" {
		return editor
	}
	if editor := strings.TrimSpace(os.Getenv("EDITOR")); editor != "" {
		return editor
	}
	return "vi"
}

func externalEditorCommand(editor, path string) *exec.Cmd {
	cmd := exec.Command("sh", "-c", `$ION_EDITOR "$ION_COMPOSER_FILE"`)
	cmd.Env = append(os.Environ(), "ION_EDITOR="+editor, "ION_COMPOSER_FILE="+path)
	return cmd
}

func writeExternalEditorBuffer(content string) (string, error) {
	file, err := os.CreateTemp("", "ion-composer-*.md")
	if err != nil {
		return "", err
	}
	path := file.Name()
	if _, err := file.WriteString(content); err != nil {
		_ = file.Close()
		_ = os.Remove(path)
		return "", err
	}
	if err := file.Close(); err != nil {
		_ = os.Remove(path)
		return "", err
	}
	return path, nil
}

// === Extracted command handlers ===

func (m Model) handleHelpCommand() (Model, tea.Cmd) {
	return m, m.terminalCommit().Help(helpText())
}

func (m Model) handleHotkeysCommand() (Model, tea.Cmd) {
	return m, m.terminalCommit().Help(hotkeysText())
}

func (m Model) handleExitCommand() (Model, tea.Cmd) {
	return m, tea.Quit
}

func (m Model) handlePrimaryCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /primary")
	}
	return m.switchPresetCommand(runtime.PresetPrimary)
}

func (m Model) handleFastCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /fast")
	}
	return m.switchPresetCommand(runtime.PresetFast)
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
	summarizer, ok := m.Model.Backend.(ToolSummarizer)
	if !ok {
		return m, cmdError("tool summary unavailable for this backend")
	}
	return m, m.terminalCommit().Entries(
		systemEntry(toolSurfaceSummary(summarizer.ToolSurface())),
	)
}

func (m Model) handleStatusCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /status")
	}
	return m, m.terminalCommit().Entries(systemEntry(runtimeStatusSummary(m)))
}

func (m Model) handleChangelogCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /changelog")
	}
	content, err := os.ReadFile("CHANGELOG.md")
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to read CHANGELOG.md: %v", err))
	}
	return m, m.terminalCommit().Entries(systemEntry(string(content)))
}

func (m Model) handleSkillsCommand(input, command string) (Model, tea.Cmd) {
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
}

func (m Model) handleNewSessionCommand(fields []string, command string) (Model, tea.Cmd) {
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
	return m.switchRuntimeCommand(
		newRuntimeTransition(appCfg, runtimeCfg, m.activePreset(), ""),
		systemEntry(notice),
		"",
		false,
	)
}

func (m Model) handleCompactCommand() (Model, tea.Cmd) {
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
}

func (m Model) handleImportCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) < 2 {
		return m, cmdError("usage: /import <filename>")
	}
	return m.importSession(fields[1])
}

func (m Model) handleNameCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) < 2 {
		return m, cmdError("usage: /name <session-name>")
	}
	return m.nameSession(strings.Join(fields[1:], " "))
}

func (m Model) handleLabelCommand(fields []string) (Model, tea.Cmd) {
	if m.Model.Session == nil {
		return m, cmdError("no active session")
	}
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	sess := m.Model.Session
	store := m.Model.Store
	leafID := store.GetLeafID()

	if len(fields) < 2 {
		// Show current label.
		return m, func() tea.Msg {
			label, err := sess.GetLabel(context.Background(), leafID)
			return labelShowMsg{label: label, err: err}
		}
	}
	// Set label.
	text := strings.Join(fields[1:], " ")
	return m, func() tea.Msg {
		_, err := sess.AppendLabel(context.Background(), leafID, text)
		return labelShowMsg{label: text, err: err}
	}
}

type labelShowMsg struct {
	label string
	err   error
}

func (m Model) handleLabelShow(msg labelShowMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		m.terminalCommit().Entries(systemEntry(fmt.Sprintf("⚠ label: %v", msg.err)))
		return m, nil
	}
	if msg.label == "" {
		m.terminalCommit().Entries(systemEntry("ℹ no label set on current branch"))
	} else {
		m.terminalCommit().Entries(systemEntry(fmt.Sprintf("🏷 label: %s", msg.label)))
	}
	return m, nil
}
