package app

import (
	"context"
	"fmt"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// handleCommand dispatches a slash command entered by the user.
func (m *Model) handleCommand(input string) tea.Cmd {
	fields := strings.Fields(input)
	if len(fields) == 0 {
		return nil
	}

	switch fields[0] {
	case "/help":
		return func() tea.Msg {
			return sessionHelpMsg{notice: helpText()}
		}

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
		cfg, err := config.Load()
		if err != nil {
			return cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		if strings.EqualFold(strings.TrimSpace(cfg.Model), strings.TrimSpace(name)) {
			return nil
		}
		cfg.Model = name
		if err := config.Save(cfg); err != nil {
			return cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(cfg)
		if cfg.Provider == "" {
			m.status = noProviderConfiguredStatus()
			return m.printEntries(session.Entry{Role: session.System, Content: "Model set to " + name})
		}
		return m.switchRuntimeCommand(cfg, session.Entry{Role: session.System, Content: "Model set to " + name}, m.session.ID(), false)

	case "/provider":
		if len(fields) < 2 {
			return m.openProviderPicker()
		}
		name := fields[1]
		cfg, err := config.Load()
		if err != nil {
			return cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		cfg.Provider = name
		cfg.Model = ""
		if err := config.Save(cfg); err != nil {
			return cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(cfg)
		m.status = noModelConfiguredStatus()
		return m.openModelPickerWithConfig(cfg)

	case "/mcp":
		if len(fields) < 3 || fields[1] != "add" {
			return cmdError("usage: /mcp add <command> [args...]")
		}
		mcpCmd := fields[2]
		mcpArgs := fields[3:]
		sess := m.session
		return func() tea.Msg {
			if err := sess.RegisterMCPServer(context.Background(), mcpCmd, mcpArgs...); err != nil {
				return session.Error{Err: err}
			}
			return nil
		}

	case "/clear":
		cfg, err := config.Load()
		if err != nil {
			return cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		if cfg.Provider == "" {
			cfg.Provider = m.backend.Provider()
		}
		if cfg.Model == "" {
			cfg.Model = m.backend.Model()
		}
		if cfg.Provider == "" || cfg.Model == "" {
			return cmdError("cannot /clear without an active provider and model")
		}
		return m.switchRuntimeCommand(cfg, session.Entry{Role: session.System, Content: "Started fresh session"}, "", false)

	case "/cost":
		inputTokens, outputTokens, totalCost := m.tokensSent, m.tokensReceived, m.totalCost
		if m.storage != nil {
			input, output, cost, err := m.storage.Usage(context.Background())
			if err != nil {
				return cmdError(fmt.Sprintf("failed to load session usage: %v", err))
			}
			inputTokens = input
			outputTokens = output
			totalCost = cost
		}
		if totalCost <= 0 {
			return func() tea.Msg {
				return sessionCostMsg{notice: "No API cost tracked for this session"}
			}
		}
		totalTokens := inputTokens + outputTokens
		return func() tea.Msg {
			return sessionCostMsg{
				notice: fmt.Sprintf(
					"Session cost\ninput tokens: %d\noutput tokens: %d\ntotal tokens: %d\ncost: $%.6f",
					inputTokens,
					outputTokens,
					totalTokens,
					totalCost,
				),
			}
		}

	case "/compact":
		compactor, ok := m.backend.(backend.Compactor)
		if !ok {
			return cmdError("current backend does not support /compact")
		}
		return func() tea.Msg {
			compacted, err := compactor.Compact(context.Background())
			if err != nil {
				return session.Error{Err: err}
			}
			if compacted {
				return sessionCompactedMsg{notice: "Compacted current session context"}
			}
			return sessionCompactedMsg{notice: "Session is already within compaction limits"}
		}

	case "/exit", "/quit":
		return tea.Quit

	default:
		return cmdError(fmt.Sprintf("unknown command: %s", fields[0]))
	}
}

func (m *Model) openProviderPicker() tea.Cmd {
	cfg, err := config.Load()
	if err != nil {
		return cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openProviderPickerWithConfig(cfg)
}

func (m *Model) openProviderPickerWithConfig(cfg *config.Config) tea.Cmd {
	items := providerItems(cfg)
	m.picker = &pickerState{
		title:    "Pick a provider",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Provider),
		purpose:  pickerPurposeProvider,
		cfg:      cfg,
	}
	return nil
}

func (m *Model) openModelPicker() tea.Cmd {
	cfg, err := config.Load()
	if err != nil {
		return cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openModelPickerWithConfig(cfg)
}

func (m *Model) openModelPickerWithConfig(cfg *config.Config) tea.Cmd {
	if cfg.Provider == "" {
		return m.openProviderPickerWithConfig(cfg)
	}
	items, err := modelItemsForProvider(cfg)
	if err != nil {
		return cmdError(fmt.Sprintf("failed to list models for %s: %v", cfg.Provider, err))
	}
	if len(items) == 0 {
		return cmdError(fmt.Sprintf("no models available for provider %s", cfg.Provider))
	}
	m.picker = &pickerState{
		title:    "Pick a model for " + cfg.Provider,
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Model),
		purpose:  pickerPurposeModel,
		cfg:      cfg,
	}
	return nil
}

func helpText() string {
	return strings.Join([]string{
		"ion commands",
		"",
		"  /resume [id]     resume a recent session or pick one",
		"  /provider [name] set provider and choose a model",
		"  /model [name]    set model directly or open the picker",
		"  /compact         compact the current session",
		"  /clear           start a fresh session with the current provider/model",
		"  /cost            show aggregate session usage",
		"  /mcp add <cmd>   register an MCP server",
		"  /quit, /exit     leave ion",
		"  /help            show this help",
		"",
		"keys",
		"",
		"  Ctrl+P           provider picker",
		"  Ctrl+M           model picker",
		"  Tab              swap provider/model pickers",
		"  Shift+Tab        toggle read/write mode",
		"  Esc              cancel turn, or clear composer on double-tap",
		"  Up / Down        command history",
		"  Enter            send message",
		"  Shift+Enter      insert newline",
		"  Alt+Enter        insert newline",
		"  Ctrl+C           clear composer, or quit on double-tap when empty",
		"  Ctrl+D           quit on double-tap when empty",
	}, "\n")
}

func (m *Model) handlePickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		m.picker = nil
		return *m, nil
	case "backspace":
		if len(m.picker.query) > 0 {
			_, size := utf8.DecodeLastRuneInString(m.picker.query)
			m.picker.query = m.picker.query[:len(m.picker.query)-size]
			refreshPickerFilter(m)
		}
		return *m, nil
	case "tab":
		if m.picker.purpose == pickerPurposeProvider {
			if m.picker.cfg != nil && m.picker.cfg.Provider != "" {
				return *m, m.openModelPickerWithConfig(m.picker.cfg)
			}
			return *m, nil
		}
		if m.picker.purpose == pickerPurposeModel {
			return *m, m.openProviderPickerWithConfig(m.picker.cfg)
		}
		return *m, nil
	case "up":
		if m.picker.index > 0 {
			m.picker.index--
		}
		return *m, nil
	case "down":
		if m.picker.index < len(pickerDisplayItems(m.picker))-1 {
			m.picker.index++
		}
		return *m, nil
	case "enter":
		return m.commitPickerSelection()
	default:
		if msg.Text != "" {
			m.picker.query += msg.Text
			refreshPickerFilter(m)
			return *m, nil
		}
		return *m, nil
	}
}

func (m *Model) commitPickerSelection() (Model, tea.Cmd) {
	items := pickerDisplayItems(m.picker)
	if m.picker == nil || len(items) == 0 {
		m.picker = nil
		return *m, nil
	}

	selected := items[m.picker.index]
	cfg := *m.picker.cfg

	switch m.picker.purpose {
	case pickerPurposeProvider:
		if strings.EqualFold(cfg.Provider, selected.Value) {
			m.picker = nil
			return *m, m.openModelPickerWithConfig(&cfg)
		}
		cfg.Provider = selected.Value
		cfg.Model = ""
		if err := config.Save(&cfg); err != nil {
			return *m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(&cfg)
		m.status = noModelConfiguredStatus()
		m.picker = nil
		return *m, m.openModelPickerWithConfig(&cfg)

	case pickerPurposeModel:
		if strings.EqualFold(strings.TrimSpace(cfg.Model), strings.TrimSpace(selected.Value)) {
			m.picker = nil
			return *m, nil
		}
		cfg.Model = selected.Value
		if err := config.Save(&cfg); err != nil {
			return *m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.picker = nil
		notice := session.Entry{Role: session.System, Content: "Model set to " + selected.Value}
		return *m, m.switchRuntimeCommand(&cfg, notice, m.session.ID(), false)
	default:
		m.picker = nil
		return *m, nil
	}
}

func (m *Model) resumeStoredSessionByID(sessionID string) tea.Cmd {
	if m.store == nil {
		return cmdError("session store not available")
	}

	resumed, err := m.store.ResumeSession(context.Background(), sessionID)
	if err != nil {
		return cmdError(fmt.Sprintf("failed to resume session %s: %v", sessionID, err))
	}
	defer func() {
		_ = resumed.Close()
	}()

	meta := resumed.Meta()
	provider, model := splitStoredSessionModel(meta.Model)
	if provider == "" || model == "" {
		return cmdError(fmt.Sprintf("session %s is missing provider/model metadata", sessionID))
	}

	cfg := &config.Config{Provider: provider, Model: model}
	notice := session.Entry{Role: session.System, Content: "Resumed session " + sessionID}
	return m.resumeRuntimeCommand(cfg, notice, sessionID)
}

func (m *Model) switchRuntimeCommand(cfg *config.Config, notice session.Entry, sessionID string, preserveSession bool) tea.Cmd {
	if m.switcher == nil {
		m.backend.SetConfig(cfg)
		return m.printEntries(notice)
	}

	oldSession := m.session
	switchID := sessionID
	if preserveSession && switchID == "" && oldSession != nil {
		switchID = oldSession.ID()
	}
	switcher := m.switcher
	cfgCopy := *cfg

	return func() tea.Msg {
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, switchID)
		if err != nil {
			return session.Error{Err: err}
		}
		if oldSession != nil {
			_ = oldSession.Close()
		}
		return runtimeSwitchedMsg{
			backend:    backend,
			session:    sess,
			storage:    storageSess,
			status:     backend.Bootstrap().Status,
			notice:     notice.Content,
			showStatus: preserveSession,
		}
	}
}

func (m *Model) resumeRuntimeCommand(cfg *config.Config, notice session.Entry, sessionID string) tea.Cmd {
	if m.switcher == nil {
		m.backend.SetConfig(cfg)
		return m.printEntries(notice)
	}
	switcher := m.switcher
	cfgCopy := *cfg
	return func() tea.Msg {
		oldSession := m.session
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, sessionID)
		if err != nil {
			return session.Error{Err: err}
		}
		if oldSession != nil {
			_ = oldSession.Close()
		}
		var entries []session.Entry
		resumeBranch := currentBranchName(m.branch, storageSess)
		if storageSess != nil {
			entries, err = storageSess.Entries(context.Background())
			if err != nil {
				return session.Error{Err: fmt.Errorf("load session transcript: %w", err)}
			}
		}
		printLines := []string{"--- resumed ---", m.runtimeHeaderLine(backend)}
		if header := m.headerLineFor(resumeBranch); header != "" {
			printLines = append(printLines, header)
		}
		return runtimeSwitchedMsg{
			backend:       backend,
			session:       sess,
			storage:       storageSess,
			printLines:    printLines,
			replayEntries: entries,
			status:        backend.Bootstrap().Status,
			notice:        notice.Content,
			showStatus:    false,
		}
	}
}

func currentBranchName(fallback string, sess storage.Session) string {
	if sess == nil {
		return fallback
	}
	if branch := strings.TrimSpace(sess.Meta().Branch); branch != "" {
		return branch
	}
	return fallback
}

func splitStoredSessionModel(value string) (string, string) {
	value = strings.TrimSpace(value)
	if value == "" {
		return "", ""
	}
	provider, model, ok := strings.Cut(value, "/")
	if !ok {
		return "", value
	}
	return strings.TrimSpace(provider), strings.TrimSpace(model)
}

// cmdError returns a Cmd that emits a session.Error with the given message.
func cmdError(msg string) tea.Cmd {
	return func() tea.Msg {
		return session.Error{Err: fmt.Errorf("%s", msg)}
	}
}

// renderDiff colorizes diff-format output.
// Falls back to plain output if the content doesn't look like a unified diff.
func (m Model) renderDiff(content string) string {
	lines := strings.Split(content, "\n")
	hasDiffMarkers := false
	for _, l := range lines {
		if strings.HasPrefix(l, "--- ") || strings.HasPrefix(l, "+++ ") ||
			strings.HasPrefix(l, "@@ ") {
			hasDiffMarkers = true
			break
		}
	}
	if !hasDiffMarkers {
		return content
	}

	var b strings.Builder
	for _, l := range lines {
		switch {
		case strings.HasPrefix(l, "+") && !strings.HasPrefix(l, "+++"):
			b.WriteString(m.st.added.Render(l))
		case strings.HasPrefix(l, "-") && !strings.HasPrefix(l, "---"):
			b.WriteString(m.st.removed.Render(l))
		case strings.HasPrefix(l, "@@ "):
			b.WriteString(m.st.cyan.Render(l))
		default:
			b.WriteString(m.st.dim.Render(l))
		}
		b.WriteString("\n")
	}
	return b.String()
}

// isWriteTool returns true if the tool title looks like a write/edit operation.
func isWriteTool(title string) bool {
	for _, prefix := range []string{"write(", "edit(", "create(", "Write(", "Edit("} {
		if strings.HasPrefix(title, prefix) {
			return true
		}
	}
	return false
}
