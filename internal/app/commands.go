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
		cfg, err := config.Load()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		currentCfg, err := m.runtimeConfigForActivePreset(cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" && strings.EqualFold(strings.TrimSpace(currentCfg.Model), strings.TrimSpace(name)) {
			return m, nil
		}
		updated := m.updateModelForActivePreset(cfg, name)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.Save(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
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
			m.activePreset(),
			session.Entry{Role: session.System, Content: "Model set to " + name},
			m.Model.Session.ID(),
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
		currentCfg, err := m.runtimeConfigForActivePreset(cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" && normalizeThinkingValue(currentCfg.ReasoningEffort) == level {
			return m, nil
		}
		updated := m.updateThinkingForActivePreset(cfg, level)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.Save(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.Model.Backend.SetConfig(runtimeCfg)
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
		cfg, err := config.Load()
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		updated := m.updateProviderForActivePreset(cfg, name)
		if err := config.Save(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.Model.Backend.SetConfig(updated)
		m.Progress.Status = noModelConfiguredStatus()
		return m.openModelPickerWithConfig(updated)

	case "/mcp":
		if len(fields) < 3 || fields[1] != "add" {
			return m, cmdError("usage: /mcp add <command> [args...]")
		}
		mcpCmd := fields[2]
		mcpArgs := fields[3:]
		sess := m.Model.Session
		return m, func() tea.Msg {
			if err := sess.RegisterMCPServer(context.Background(), mcpCmd, mcpArgs...); err != nil {
				return session.Error{Err: err}
			}
			return nil
		}

	case "/yolo":
		if m.Mode == session.ModeYolo {
			m.Mode = session.ModeEdit
			m.Model.Session.SetMode(m.Mode)
			m.Model.Session.SetAutoApprove(false)
			return m, m.printEntries(session.Entry{Role: session.System, Content: "Mode: EDIT"})
		}
		m.Mode = session.ModeYolo
		m.Model.Session.SetMode(m.Mode)
		m.Model.Session.SetAutoApprove(true)
		return m, m.printEntries(session.Entry{Role: session.System, Content: "Mode: YOLO"})

	case "/mode":
		if len(fields) < 2 {
			modeName := modeDisplayName(m.Mode)
			return m, m.printEntries(
				session.Entry{Role: session.System, Content: "Current mode: " + modeName},
			)
		}
		switch strings.ToLower(fields[1]) {
		case "read", "r":
			m.Mode = session.ModeRead
		case "edit", "e", "write", "w":
			m.Mode = session.ModeEdit
		case "yolo", "y":
			m.Mode = session.ModeYolo
		default:
			return m, cmdError("usage: /mode [read|edit|yolo]")
		}
		m.Model.Session.SetMode(m.Mode)
		m.Model.Session.SetAutoApprove(m.Mode == session.ModeYolo)
		return m, m.printEntries(
			session.Entry{Role: session.System, Content: "Mode: " + modeDisplayName(m.Mode)},
		)

	case "/clear":
		cfg, err := config.Load()
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
			return m, cmdError("cannot /clear without an active provider and model")
		}
		return m, m.switchRuntimeCommand(
			runtimeCfg,
			m.activePreset(),
			session.Entry{Role: session.System, Content: "Started fresh session"},
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
			if m.Model.Config != nil && (m.Model.Config.MaxSessionCost > 0 || m.Model.Config.MaxTurnCost > 0) {
				return m, func() tea.Msg {
					return sessionCostMsg{notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost)}
				}
			}
			return m, func() tea.Msg {
				return sessionCostMsg{notice: "No API cost tracked for this session"}
			}
		}
		return m, func() tea.Msg {
			return sessionCostMsg{notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost)}
		}

	case "/compact":
		compactor, ok := m.Model.Backend.(backend.Compactor)
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

func (m Model) openProviderPicker() (Model, tea.Cmd) {
	cfg, err := config.Load()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openProviderPickerWithConfig(cfg)
}

func (m Model) openProviderPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	items := providerItems(cfg)
	m.Picker.Overlay = &pickerOverlayState{
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
	runtimeCfg, err := m.runtimeConfigForActivePreset(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	favorites := m.modelPickerFavoriteItems(cfg, items)
	catalog := m.modelPickerCatalogItems(items, favorites)
	combined := append(clonePickerItems(favorites), catalog...)
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a " + m.activePresetTitle() + " model for " + cfg.Provider,
		items:    combined,
		filtered: clonePickerItems(combined),
		index:    pickerIndex(combined, runtimeCfg.Model),
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
		"  /primary         switch to the primary model preset",
		"  /fast            switch to the fast model preset",
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
		"  Ctrl+M           toggle primary/fast preset",
		"  Ctrl+T           thinking picker",
		"  Tab              swap provider/model pickers",
		"  PgUp / PgDn      page through picker lists",
		"  Shift+Tab        cycle READ → EDIT → YOLO",
		"  Esc              cancel running turn",
		"  Up / Down        command history",
		"  Ctrl+P / Ctrl+N  command history",
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
	runtimeCfg, err := m.runtimeConfigForActivePreset(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	items := []pickerItem{
		{Label: "Auto", Value: config.DefaultReasoningEffort, Detail: "Provider default"},
		{Label: "Low", Value: "low"},
		{Label: "Medium", Value: "medium"},
		{Label: "High", Value: "high"},
	}
	for i := range items {
		items[i].Search = pickerSearchIndex(
			items[i].Label,
			items[i].Value,
			items[i].Detail,
			"",
			nil,
		)
	}
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a " + m.activePresetTitle() + " thinking level",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    pickerIndex(items, normalizeThinkingValue(runtimeCfg.ReasoningEffort)),
		purpose:  pickerPurposeThinking,
		cfg:      cfg,
	}
	return m, nil
}

func (m Model) modelPickerFavoriteItems(cfg *config.Config, all []pickerItem) []pickerItem {
	if cfg == nil || cfg.Provider == "" {
		return nil
	}

	primaryCfg, err := m.runtimeConfigForPreset(cfg, presetPrimary)
	if err != nil {
		return nil
	}
	fastCfg, err := m.runtimeConfigForPreset(cfg, presetFast)
	if err != nil {
		return nil
	}

	primaryModel := strings.TrimSpace(primaryCfg.Model)
	fastModel := strings.TrimSpace(fastCfg.Model)
	switch {
	case primaryModel == "" && fastModel == "":
		return nil
	case primaryModel != "" && strings.EqualFold(primaryModel, fastModel):
		item := m.modelPickerFavoriteItem(all, primaryModel)
		item.Group = "Favorites"
		item.Detail = "Primary / Fast"
		return []pickerItem{item}
	}

	favorites := make([]pickerItem, 0, 2)
	if primaryModel != "" {
		item := m.modelPickerFavoriteItem(all, primaryModel)
		item.Group = "Favorites"
		item.Detail = "Primary"
		favorites = append(favorites, item)
	}
	if fastModel != "" {
		item := m.modelPickerFavoriteItem(all, fastModel)
		item.Group = "Favorites"
		item.Detail = "Fast"
		favorites = append(favorites, item)
	}
	return favorites
}

func (m Model) modelPickerCatalogItems(all, favorites []pickerItem) []pickerItem {
	if len(all) == 0 {
		return nil
	}

	catalog := make([]pickerItem, 0, len(all))
	seen := make(map[string]struct{}, len(favorites))
	for _, item := range favorites {
		if item.Value == "" {
			continue
		}
		key := strings.ToLower(item.Value)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
	}
	for _, item := range all {
		if item.Value == "" {
			continue
		}
		key := strings.ToLower(item.Value)
		if _, ok := seen[key]; ok {
			continue
		}
		item.Group = "All models"
		catalog = append(catalog, item)
	}
	return catalog
}

func (m Model) modelPickerFavoriteItem(all []pickerItem, model string) pickerItem {
	if item, ok := pickerItemByValue(all, model); ok {
		return item
	}
	return pickerItem{
		Label:  model,
		Value:  model,
		Detail: "Configured preset",
		Tone:   pickerToneWarn,
	}
}

func togglePreset(p modelPreset) modelPreset {
	if p == presetFast {
		return presetPrimary
	}
	return presetFast
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
		m.Picker.Overlay = nil
		return m, nil
	case "backspace":
		if len(m.Picker.Overlay.query) > 0 {
			_, size := utf8.DecodeLastRuneInString(m.Picker.Overlay.query)
			m.Picker.Overlay.query = m.Picker.Overlay.query[:len(m.Picker.Overlay.query)-size]
			refreshPickerFilter(&m)
		}
		return m, nil
	case "tab":
		if m.Picker.Overlay.purpose == pickerPurposeProvider {
			if m.Picker.Overlay.cfg != nil && m.Picker.Overlay.cfg.Provider != "" {
				runtimeCfg, err := m.runtimeConfigForActivePreset(m.Picker.Overlay.cfg)
				if err != nil {
					return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
				}
				return m.openModelPickerWithConfig(runtimeCfg)
			}
			return m, nil
		}
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			return m.openProviderPickerWithConfig(m.Picker.Overlay.cfg)
		}
		return m, nil
	case "ctrl+m":
		if m.Picker.Overlay.purpose == pickerPurposeModel {
			return m.switchPresetCommand(togglePreset(m.activePreset()))
		}
		return m, nil
	case "pgup", "pageup":
		if m.Picker.Overlay.index > 0 {
			m.Picker.Overlay.index -= pickerPageSize
			if m.Picker.Overlay.index < 0 {
				m.Picker.Overlay.index = 0
			}
		}
		return m, nil
	case "pgdown", "pagedown":
		if max := len(pickerDisplayItems(m.Picker.Overlay)); max > 0 {
			m.Picker.Overlay.index += pickerPageSize
			if m.Picker.Overlay.index >= max {
				m.Picker.Overlay.index = max - 1
			}
		}
		return m, nil
	case "up":
		if m.Picker.Overlay.index > 0 {
			m.Picker.Overlay.index--
		}
		return m, nil
	case "down":
		if m.Picker.Overlay.index < len(pickerDisplayItems(m.Picker.Overlay))-1 {
			m.Picker.Overlay.index++
		}
		return m, nil
	case "enter":
		return m.commitPickerSelection()
	default:
		if msg.Text != "" {
			m.Picker.Overlay.query += msg.Text
			refreshPickerFilter(&m)
			return m, nil
		}
		return m, nil
	}
}

func (m Model) commitPickerSelection() (Model, tea.Cmd) {
	if m.Picker.Overlay == nil {
		return m, nil
	}
	items := pickerDisplayItems(m.Picker.Overlay)
	if len(items) == 0 {
		m.Picker.Overlay = nil
		return m, nil
	}

	selected := items[m.Picker.Overlay.index]
	cfg := *m.Picker.Overlay.cfg

	switch m.Picker.Overlay.purpose {
	case pickerPurposeProvider:
		if def, ok := providers.Lookup(selected.Value); ok && def.ID == "local-api" {
			if _, ready := providers.CredentialStateContext(context.Background(), cfgForProvider(&cfg, def.ID), def); !ready {
				return m, cmdError("Local API is not running")
			}
		}
		if strings.EqualFold(cfg.Provider, selected.Value) {
			m.Picker.Overlay = nil
			return m.openModelPickerWithConfig(&cfg)
		}
		updated := m.updateProviderForActivePreset(&cfg, selected.Value)
		if err := config.Save(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.Model.Backend.SetConfig(updated)
		m.Progress.Status = noModelConfiguredStatus()
		m.Picker.Overlay = nil
		return m.openModelPickerWithConfig(updated)

	case pickerPurposeModel:
		currentCfg, err := m.runtimeConfigForActivePreset(&cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" && strings.EqualFold(strings.TrimSpace(currentCfg.Model), strings.TrimSpace(selected.Value)) {
			m.Picker.Overlay = nil
			return m, nil
		}
		updated := m.updateModelForActivePreset(&cfg, selected.Value)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.Save(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.Picker.Overlay = nil
		notice := session.Entry{Role: session.System, Content: "Model set to " + selected.Value}
		return m, m.switchRuntimeCommand(runtimeCfg, m.activePreset(), notice, m.Model.Session.ID(), false)
	case pickerPurposeThinking:
		level := normalizeThinkingValue(selected.Value)
		currentCfg, err := m.runtimeConfigForActivePreset(&cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" && normalizeThinkingValue(currentCfg.ReasoningEffort) == level {
			m.Picker.Overlay = nil
			return m, nil
		}
		updated := m.updateThinkingForActivePreset(&cfg, level)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.Save(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.Model.Backend.SetConfig(runtimeCfg)
		m.Progress.ReasoningEffort = level
		m.Picker.Overlay = nil
		return m, m.printEntries(
			session.Entry{
				Role:    session.System,
				Content: "Thinking set to " + thinkingDisplayName(level),
			},
		)
	default:
		m.Picker.Overlay = nil
		return m, nil
	}
}

func (m Model) resumeStoredSessionByID(sessionID string) tea.Cmd {
	if m.Model.Store == nil {
		return cmdError("session store not available")
	}

	resumed, err := m.Model.Store.ResumeSession(context.Background(), sessionID)
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

func (m Model) switchPresetCommand(preset modelPreset) (Model, tea.Cmd) {
	cfg, err := config.Load()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForPreset(cfg, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve %s preset: %v", preset, err))
	}
	notice := session.Entry{Role: session.System, Content: "Switched to " + preset.String()}
	return m, m.switchRuntimeCommand(runtimeCfg, preset, notice, m.Model.Session.ID(), false)
}

func (m Model) switchRuntimeCommand(
	cfg *config.Config,
	preset modelPreset,
	notice session.Entry,
	sessionID string,
	preserveSession bool,
) tea.Cmd {
	if m.Model.Switcher == nil {
		m.Model.Backend.SetConfig(cfg)
		m.App.ActivePreset = preset
		m.Progress.ReasoningEffort = normalizeThinkingValue(cfg.ReasoningEffort)
		return m.printEntries(notice)
	}

	oldSession := m.Model.Session
	switchID := sessionID
	if preserveSession && switchID == "" && oldSession != nil {
		switchID = oldSession.ID()
	}
	switcher := m.Model.Switcher
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
			cfg:        &cfgCopy,
			preset:     preset,
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
	if m.Model.Switcher == nil {
		m.Model.Backend.SetConfig(cfg)
		m.App.ActivePreset = presetPrimary
		m.Progress.ReasoningEffort = normalizeThinkingValue(cfg.ReasoningEffort)
		return m.printEntries(notice)
	}
	switcher := m.Model.Switcher
	cfgCopy := *cfg
	return func() tea.Msg {
		oldSession := m.Model.Session
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
		resumeBranch := currentBranchName(m.App.Branch, storageSess)
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
			cfg:           &cfgCopy,
			preset:        presetPrimary,
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
