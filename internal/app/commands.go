package app

import (
	"context"
	"fmt"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// handleCommand dispatches a slash command entered by the user.
func (m Model) handleCommand(input string) (Model, tea.Cmd) {
	fields := strings.Fields(input)
	if len(fields) == 0 {
		return m, nil
	}

	switch fields[0] {
	case "/help":
		return m, func() tea.Msg {
			return sessionHelpMsg{notice: helpText()}
		}

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
		cfg, err := config.Load()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		if strings.EqualFold(strings.TrimSpace(cfg.Model), strings.TrimSpace(name)) {
			return m, nil
		}
		cfg.Model = name
		if err := config.Save(cfg); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(cfg)
		if cfg.Provider == "" {
			m.status = noProviderConfiguredStatus()
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Model set to " + name},
			)
		}
		return m, m.switchRuntimeCommand(
			cfg,
			session.Entry{Role: session.System, Content: "Model set to " + name},
			m.session.ID(),
			false,
		)

	case "/thinking":
		if len(fields) < 2 {
			return m.openThinkingPicker()
		}
		level := normalizeThinkingValue(fields[1])
		cfg, err := config.Load()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		if cfg.ReasoningEffort == level {
			return m, nil
		}
		cfg.ReasoningEffort = level
		if err := config.Save(cfg); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(cfg)
		m.reasoningEffort = level
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
		cfg, err := config.Load()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		cfg.Provider = name
		cfg.Model = ""
		if err := config.Save(cfg); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(cfg)
		m.status = noModelConfiguredStatus()
		return m.openModelPickerWithConfig(cfg)

	case "/mcp":
		if len(fields) < 3 || fields[1] != "add" {
			return m, cmdError("usage: /mcp add <command> [args...]")
		}
		mcpCmd := fields[2]
		mcpArgs := fields[3:]
		sess := m.session
		return m, func() tea.Msg {
			if err := sess.RegisterMCPServer(context.Background(), mcpCmd, mcpArgs...); err != nil {
				return session.Error{Err: err}
			}
			return nil
		}

	case "/yolo":
		if m.mode == session.ModeYolo {
			m.mode = session.ModeEdit
			m.session.SetMode(m.mode)
			m.session.SetAutoApprove(false)
			return m, m.printEntries(session.Entry{Role: session.System, Content: "Mode: EDIT"})
		}
		m.mode = session.ModeYolo
		m.session.SetMode(m.mode)
		m.session.SetAutoApprove(true)
		return m, m.printEntries(session.Entry{Role: session.System, Content: "Mode: YOLO"})

	case "/mode":
		if len(fields) < 2 {
			modeName := modeDisplayName(m.mode)
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Current mode: " + modeName},
			)
		}
		switch strings.ToLower(fields[1]) {
		case "read", "r":
			m.mode = session.ModeRead
		case "edit", "e", "write", "w":
			m.mode = session.ModeEdit
		case "yolo", "y":
			m.mode = session.ModeYolo
		default:
			return m, cmdError("usage: /mode [read|edit|yolo]")
		}
		m.session.SetMode(m.mode)
		m.session.SetAutoApprove(m.mode == session.ModeYolo)
		return m, m.printEntries(
			session.Entry{Role: session.System, Content: "Mode: " + modeDisplayName(m.mode)},
		)

	case "/clear":
		cfg, err := config.Load()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		if cfg.Provider == "" {
			cfg.Provider = m.backend.Provider()
		}
		if cfg.Model == "" {
			cfg.Model = m.backend.Model()
		}
		if cfg.Provider == "" || cfg.Model == "" {
			return m, cmdError("cannot /clear without an active provider and model")
		}
		return m, m.switchRuntimeCommand(
			cfg,
			session.Entry{Role: session.System, Content: "Started fresh session"},
			"",
			false,
		)

	case "/cost":
		inputTokens, outputTokens, totalCost := m.tokensSent, m.tokensReceived, m.totalCost
		if m.storage != nil {
			input, output, cost, err := m.storage.Usage(context.Background())
			if err != nil {
				return m, cmdError(fmt.Sprintf("failed to load session usage: %v", err))
			}
			inputTokens = input
			outputTokens = output
			totalCost = cost
		}
		if totalCost <= 0 {
			return m, func() tea.Msg {
				return sessionCostMsg{notice: "No API cost tracked for this session"}
			}
		}
		totalTokens := inputTokens + outputTokens
		return m, func() tea.Msg {
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
			return m, cmdError("current backend does not support /compact")
		}
		return m, func() tea.Msg {
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
		return m, tea.Quit

	default:
		return m, cmdError(fmt.Sprintf("unknown command: %s", fields[0]))
	}
}

func (m Model) openProviderPicker() (Model, tea.Cmd) {
	cfg, err := config.Load()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openProviderPickerWithConfig(cfg)
}

func (m Model) openProviderPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	items := providerItems(cfg)
	m.picker = &pickerState{
		title:    "Pick a provider",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Provider),
		purpose:  pickerPurposeProvider,
		cfg:      cfg,
	}
	return m, nil
}

func (m Model) openModelPicker() (Model, tea.Cmd) {
	cfg, err := config.Load()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openModelPickerWithConfig(cfg)
}

func (m Model) openModelPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	if cfg.Provider == "" {
		return m.openProviderPickerWithConfig(cfg)
	}
	items, err := modelItemsForProvider(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to list models for %s: %v", cfg.Provider, err))
	}
	if len(items) == 0 {
		return m, cmdError(fmt.Sprintf("no models available for provider %s", cfg.Provider))
	}
	m.picker = &pickerState{
		title:    "Pick a model for " + cfg.Provider,
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, cfg.Model),
		purpose:  pickerPurposeModel,
		cfg:      cfg,
	}
	return m, nil
}

func helpText() string {
	return strings.Join([]string{
		"ion commands",
		"",
		"  /resume [id]     resume a recent session or pick one",
		"  /provider [name] set provider and choose a model",
		"  /model [name]    set model directly or open the picker",
		"  /thinking [lvl]  set thinking: auto, low, medium, high",
		"  /yolo            toggle YOLO mode (auto-approve all)",
		"  /mode [mode]     set mode: read, edit, yolo",
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
		"  Ctrl+T           thinking picker",
		"  Tab              swap provider/model pickers",
		"  Shift+Tab        cycle READ → EDIT → YOLO",
		"  Esc              cancel turn, or clear composer on double-tap",
		"  Up / Down        command history",
		"  Enter            send message",
		"  Shift+Enter      insert newline",
		"  Alt+Enter        insert newline",
		"  Ctrl+C           clear composer, or quit on double-tap when empty",
		"  Ctrl+D           quit on double-tap when empty",
		"",
		"approval",
		"",
		"  y                approve the tool call",
		"  n                deny the tool call",
		"  a                approve and auto-approve all remaining this session",
	}, "\n")
}

func (m Model) openThinkingPicker() (Model, tea.Cmd) {
	cfg, err := config.Load()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	items := []pickerItem{
		{Label: "Auto", Value: config.DefaultReasoningEffort, Detail: "Provider default"},
		{Label: "Low", Value: "low"},
		{Label: "Medium", Value: "medium"},
		{Label: "High", Value: "high"},
	}
	current := normalizeThinkingValue(cfg.ReasoningEffort)
	for i := range items {
		items[i].Search = pickerSearchIndex(
			items[i].Label,
			items[i].Value,
			items[i].Detail,
			"",
			nil,
		)
	}
	m.picker = &pickerState{
		title:    "Pick a thinking level",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, current),
		purpose:  pickerPurposeThinking,
		cfg:      cfg,
	}
	return m, nil
}

func normalizeThinkingValue(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", config.DefaultReasoningEffort:
		return config.DefaultReasoningEffort
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	default:
		return config.DefaultReasoningEffort
	}
}

func thinkingDisplayName(value string) string {
	switch normalizeThinkingValue(value) {
	case "low":
		return "Low"
	case "medium":
		return "Medium"
	case "high":
		return "High"
	default:
		return "Auto"
	}
}

func (m Model) handlePickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		m.picker = nil
		return m, nil
	case "backspace":
		if len(m.picker.query) > 0 {
			_, size := utf8.DecodeLastRuneInString(m.picker.query)
			m.picker.query = m.picker.query[:len(m.picker.query)-size]
			refreshPickerFilter(&m)
		}
		return m, nil
	case "tab":
		if m.picker.purpose == pickerPurposeProvider {
			if m.picker.cfg != nil && m.picker.cfg.Provider != "" {
				return m.openModelPickerWithConfig(m.picker.cfg)
			}
			return m, nil
		}
		if m.picker.purpose == pickerPurposeModel {
			return m.openProviderPickerWithConfig(m.picker.cfg)
		}
		return m, nil
	case "up":
		if m.picker.index > 0 {
			m.picker.index--
		}
		return m, nil
	case "down":
		if m.picker.index < len(pickerDisplayItems(m.picker))-1 {
			m.picker.index++
		}
		return m, nil
	case "enter":
		return m.commitPickerSelection()
	default:
		if msg.Text != "" {
			m.picker.query += msg.Text
			refreshPickerFilter(&m)
			return m, nil
		}
		return m, nil
	}
}

func (m Model) commitPickerSelection() (Model, tea.Cmd) {
	items := pickerDisplayItems(m.picker)
	if m.picker == nil || len(items) == 0 {
		m.picker = nil
		return m, nil
	}

	selected := items[m.picker.index]
	cfg := *m.picker.cfg

	switch m.picker.purpose {
	case pickerPurposeProvider:
		if def, ok := providers.Lookup(selected.Value); ok && def.ID == "local-api" {
			if _, ready := providers.CredentialStateContext(context.Background(), cfgForProvider(&cfg, def.ID), def); !ready {
				return m, cmdError("Local API is not running")
			}
		}
		if strings.EqualFold(cfg.Provider, selected.Value) {
			m.picker = nil
			return m.openModelPickerWithConfig(&cfg)
		}
		cfg.Provider = selected.Value
		cfg.Model = ""
		if err := config.Save(&cfg); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(&cfg)
		m.status = noModelConfiguredStatus()
		m.picker = nil
		return m.openModelPickerWithConfig(&cfg)

	case pickerPurposeModel:
		if strings.EqualFold(strings.TrimSpace(cfg.Model), strings.TrimSpace(selected.Value)) {
			m.picker = nil
			return m, nil
		}
		cfg.Model = selected.Value
		if err := config.Save(&cfg); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.picker = nil
		notice := session.Entry{Role: session.System, Content: "Model set to " + selected.Value}
		return m, m.switchRuntimeCommand(&cfg, notice, m.session.ID(), false)
	case pickerPurposeThinking:
		level := normalizeThinkingValue(selected.Value)
		if normalizeThinkingValue(cfg.ReasoningEffort) == level {
			m.picker = nil
			return m, nil
		}
		cfg.ReasoningEffort = level
		if err := config.Save(&cfg); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(&cfg)
		m.reasoningEffort = level
		m.picker = nil
		return m, m.printEntries(
			session.Entry{
				Role:    session.System,
				Content: "Thinking set to " + thinkingDisplayName(level),
			},
		)
	default:
		m.picker = nil
		return m, nil
	}
}

func (m Model) resumeStoredSessionByID(sessionID string) tea.Cmd {
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

func (m Model) switchRuntimeCommand(
	cfg *config.Config,
	notice session.Entry,
	sessionID string,
	preserveSession bool,
) tea.Cmd {
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

func (m Model) resumeRuntimeCommand(
	cfg *config.Config,
	notice session.Entry,
	sessionID string,
) tea.Cmd {
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

func currentBranchName(defaultBranch string, sess storage.Session) string {
	if sess == nil {
		return defaultBranch
	}
	if branch := strings.TrimSpace(sess.Meta().Branch); branch != "" {
		return branch
	}
	return defaultBranch
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

func modeDisplayName(mode session.Mode) string {
	switch mode {
	case session.ModeRead:
		return "READ"
	case session.ModeEdit:
		return "EDIT"
	case session.ModeYolo:
		return "YOLO"
	default:
		return "EDIT"
	}
}
