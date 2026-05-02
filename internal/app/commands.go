package app

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
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

	case "/memory":
		explorer, ok := m.Model.Backend.(backend.MemoryExplorer)
		if !ok {
			return m, cmdError("memory view unavailable for this backend")
		}
		query := strings.TrimSpace(strings.TrimPrefix(input, command))
		out, err := explorer.MemoryView(context.Background(), query)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to load memory: %v", err))
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

func pickerSelectionRequiresIdle(purpose pickerPurpose) bool {
	switch purpose {
	case pickerPurposeProvider, pickerPurposeModel, pickerPurposeThinking:
		return true
	default:
		return false
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

func (m Model) sessionInfoNotice() (string, error) {
	sessionID := ""
	if m.Model.Storage != nil {
		if storage.IsMaterialized(m.Model.Storage) {
			sessionID = strings.TrimSpace(m.Model.Storage.ID())
		}
	} else if m.Model.Session != nil {
		sessionID = strings.TrimSpace(m.Model.Session.ID())
	}
	if sessionID == "" {
		sessionID = "none"
	}

	provider := strings.TrimSpace(m.Model.Backend.Provider())
	model := strings.TrimSpace(m.Model.Backend.Model())
	if provider == "" {
		provider = "unknown"
	}
	if model == "" {
		model = "unknown"
	}

	inputTokens, outputTokens, totalCost := m.Progress.TokensSent, m.Progress.TokensReceived, m.Progress.TotalCost
	var entries []session.Entry
	if m.Model.Storage != nil {
		input, output, cost, err := m.Model.Storage.Usage(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session usage: %v", err)
		}
		inputTokens = input
		outputTokens = output
		totalCost = cost
		loaded, err := m.Model.Storage.Entries(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session entries: %v", err)
		}
		entries = loaded
	}

	counts := sessionEntryCounts(entries)
	lines := []string{
		"Session",
		"id: " + sessionID,
		"provider: " + provider,
		"model: " + model,
		"mode: " + modeDisplayName(m.Mode),
	}
	if branch := strings.TrimSpace(m.App.Branch); branch != "" {
		lines = append(lines, "branch: "+branch)
	}
	lines = append(lines,
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
		switch entry.Role {
		case session.User:
			counts.user++
		case session.Agent:
			counts.agent++
		case session.Tool:
			counts.tool++
		}
	}
	return counts
}

func (m Model) rewindCheckpointCommand(id string, confirmed bool) (Model, tea.Cmd) {
	if m.Model.Checkpoints == nil {
		return m, cmdError("checkpoint store is unavailable")
	}
	cp, err := m.Model.Checkpoints.Load(id)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load checkpoint: %v", err))
	}
	if !sameWorkspace(cp.Workspace, m.App.Workdir) {
		return m, cmdError("checkpoint belongs to a different workspace")
	}
	plan, err := m.Model.Checkpoints.AnalyzeRestore(context.Background(), cp)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to analyze checkpoint: %v", err))
	}
	if len(plan.Conflicts) == 0 {
		return m, m.printEntries(session.Entry{
			Role: session.System,
			Content: fmt.Sprintf(
				"Checkpoint %s already matches this workspace; nothing to rewind.",
				cp.ID,
			),
		})
	}
	if !confirmed {
		return m, m.printEntries(session.Entry{
			Role:    session.System,
			Content: rewindPreview(cp.ID, plan),
		})
	}

	before := session.Entry{
		Role: session.System,
		Content: fmt.Sprintf(
			"Rewind starting: checkpoint %s will restore %d path(s).",
			cp.ID,
			len(plan.Conflicts),
		),
	}
	report, err := m.Model.Checkpoints.Restore(
		context.Background(),
		cp,
		ionworkspace.RestoreOptions{AllowConflicts: true},
	)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to restore checkpoint: %v", err))
	}
	after := session.Entry{Role: session.System, Content: rewindReport(cp.ID, report)}
	return m, m.printEntries(before, after)
}

func sameWorkspace(a, b string) bool {
	aAbs, err := filepath.Abs(a)
	if err != nil {
		return false
	}
	bAbs, err := filepath.Abs(b)
	if err != nil {
		return false
	}
	return filepath.Clean(aAbs) == filepath.Clean(bAbs)
}

func rewindPreview(id string, plan ionworkspace.RestorePlan) string {
	lines := []string{
		"Rewind preview: " + id,
		fmt.Sprintf("%d path(s) would change.", len(plan.Conflicts)),
		"Run /rewind " + id + " --confirm to restore this checkpoint.",
		"",
	}
	for i, conflict := range plan.Conflicts {
		if i == 12 {
			lines = append(lines, fmt.Sprintf("... and %d more", len(plan.Conflicts)-i))
			break
		}
		lines = append(lines, fmt.Sprintf(
			"- %s %s (current: %s, checkpoint: %s)",
			conflict.Action,
			conflict.Path,
			conflict.Current,
			conflict.Target,
		))
	}
	return strings.Join(lines, "\n")
}

func rewindReport(id string, report ionworkspace.RestoreReport) string {
	lines := []string{
		"Rewind complete: " + id,
		fmt.Sprintf("restored: %d", len(report.Restored)),
		fmt.Sprintf("removed: %d", len(report.Removed)),
	}
	return strings.Join(lines, "\n")
}

func (m Model) openProviderPicker() (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openProviderPickerWithConfig(cfg)
}

func (m Model) openProviderPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	items := providerItems(cfg)
	m.clearProgressError()
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
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openModelPickerWithConfig(cfg)
}

func (m Model) openModelPickerWithConfig(cfg *config.Config) (Model, tea.Cmd) {
	if cfg.Provider == "" {
		return m.openProviderPickerWithConfig(cfg)
	}
	if !providers.SupportsModelListing(cfg) {
		return m, cmdError(providerModelEntryNotice(cfg.Provider))
	}
	items, err := modelItemsForProvider(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to list models for %s: %v", cfg.Provider, err))
	}
	if len(items) == 0 {
		return m, cmdError(fmt.Sprintf("no models available for provider %s", cfg.Provider))
	}
	favorites := m.modelPickerFavoriteItems(cfg, items)
	catalog := m.modelPickerCatalogItems(items, favorites)
	combined := append(clonePickerItems(favorites), catalog...)
	m.clearProgressError()
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a " + m.activePresetTitle() + " model for " + cfg.Provider,
		items:    combined,
		filtered: clonePickerItems(combined),
		index:    pickerIndex(combined, m.configuredModelForActivePreset(cfg)),
		purpose:  pickerPurposeModel,
		cfg:      cfg,
	}
	return m, nil
}

func (m Model) openThinkingPicker() (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForActivePreset(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	items := []pickerItem{
		{Label: "Auto", Value: config.DefaultReasoningEffort, Detail: "Provider default"},
		{Label: "Off", Value: "off"},
		{Label: "Minimal", Value: "minimal"},
		{Label: "Low", Value: "low"},
		{Label: "Medium", Value: "medium"},
		{Label: "High", Value: "high"},
		{Label: "XHigh", Value: "xhigh"},
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

	primaryModel := strings.TrimSpace(cfg.Model)
	fastModel := strings.TrimSpace(cfg.FastModel)
	switch {
	case primaryModel == "" && fastModel == "":
		return nil
	case primaryModel != "" && strings.EqualFold(primaryModel, fastModel):
		item := m.modelPickerFavoriteItem(all, primaryModel)
		item.Group = "Configured presets"
		return []pickerItem{item}
	}

	favorites := make([]pickerItem, 0, 2)
	if primaryModel != "" {
		item := m.modelPickerFavoriteItem(all, primaryModel)
		item.Group = "Configured presets"
		favorites = append(favorites, item)
	}
	if fastModel != "" {
		item := m.modelPickerFavoriteItem(all, fastModel)
		item.Group = "Configured presets"
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
		Label:   model,
		Value:   model,
		Detail:  "metadata unavailable",
		Tone:    pickerToneWarn,
		Metrics: &pickerMetrics{Context: "—", Input: "—", Output: "—"},
		Search: pickerSearchIndex(
			model,
			model,
			"metadata unavailable",
			"Configured presets",
			&pickerMetrics{Context: "—", Input: "—", Output: "—"},
		),
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
	case "off", "none", "disabled":
		return "off"
	case "minimal", "min":
		return "minimal"
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	case "xhigh", "extra-high", "extra_high", "extra high":
		return "xhigh"
	case "max", "maximum":
		return "max"
	default:
		return config.DefaultReasoningEffort
	}
}

func thinkingDisplayName(value string) string {
	switch normalizeThinkingValue(value) {
	case "off":
		return "Off"
	case "minimal":
		return "Minimal"
	case "low":
		return "Low"
	case "medium":
		return "Medium"
	case "high":
		return "High"
	case "xhigh":
		return "XHigh"
	case "max":
		return "Max"
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
			m.App.ActivePreset = togglePreset(m.activePreset())
			if err := config.SaveActivePreset(m.App.ActivePreset.String()); err != nil {
				return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
			}
			return m.openModelPickerWithConfig(m.Picker.Overlay.cfg)
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
	var cfg config.Config
	if m.Picker.Overlay.cfg != nil {
		cfg = *m.Picker.Overlay.cfg
	}
	if m.localCommandBusy() && pickerSelectionRequiresIdle(m.Picker.Overlay.purpose) {
		m.Picker.Overlay = nil
		return m, cmdError("Finish or cancel the current turn before changing runtime settings.")
	}

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
		if err := config.SaveState(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Model.Backend.SetConfig(updated)
		m.Model.Config = updated
		m.clearProgressError()
		m.Progress.Status = noModelConfiguredStatus()
		m.Picker.Overlay = nil
		if !providers.SupportsModelListing(updated) {
			return m, m.printEntries(session.Entry{
				Role:    session.System,
				Content: providerModelEntryNotice(updated.Provider),
			})
		}
		return m.openModelPickerWithConfig(updated)

	case pickerPurposeModel:
		currentCfg, err := m.runtimeConfigForActivePreset(&cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" &&
			strings.EqualFold(
				strings.TrimSpace(currentCfg.Model),
				strings.TrimSpace(selected.Value),
			) {
			m.Picker.Overlay = nil
			return m, nil
		}
		updated := m.updateModelForActivePreset(&cfg, selected.Value)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.SaveState(updated); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
		}
		m.Picker.Overlay = nil
		notice := session.Entry{Role: session.System, Content: "Model set to " + selected.Value}
		return m, m.switchRuntimeCommand(
			runtimeCfg,
			updated,
			m.activePreset(),
			notice,
			m.currentMaterializedSessionID(),
			false,
		)
	case pickerPurposeThinking:
		level := normalizeThinkingValue(selected.Value)
		currentCfg, err := m.runtimeConfigForActivePreset(&cfg)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if currentCfg.Provider != "" &&
			normalizeThinkingValue(currentCfg.ReasoningEffort) == level {
			m.Picker.Overlay = nil
			return m, nil
		}
		updated := m.updateThinkingForActivePreset(&cfg, level)
		runtimeCfg, err := m.runtimeConfigForActivePreset(updated)
		if err != nil {
			return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
		}
		if err := config.SaveReasoningState(m.activePreset().String(), level); err != nil {
			return m, cmdError(fmt.Sprintf("failed to save state: %v", err))
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
	case pickerPurposeCommand:
		m.Input.Composer.SetValue(selected.Value + " ")
		m.relayoutComposer()
		m.Picker.Overlay = nil
		return m, nil
	default:
		m.Picker.Overlay = nil
		return m, nil
	}
}

func providerModelEntryNotice(provider string) string {
	display := providerDisplayName(provider)
	if strings.TrimSpace(display) == "" {
		display = provider
	}
	return display + " does not provide a model list. Set a model with /model <id>."
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
	return false
}

func (m Model) switchPresetCommand(preset modelPreset) (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForPreset(cfg, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve %s preset: %v", preset, err))
	}
	notice := session.Entry{Role: session.System, Content: "Switched to " + preset.String()}
	return m, m.switchRuntimeCommand(
		runtimeCfg,
		cfg,
		preset,
		notice,
		m.currentMaterializedSessionID(),
		false,
	)
}

func (m Model) currentMaterializedSessionID() string {
	if m.Model.Session == nil {
		return ""
	}
	if m.Model.Storage == nil {
		return m.Model.Session.ID()
	}
	if !storage.IsMaterialized(m.Model.Storage) {
		return ""
	}
	return m.Model.Session.ID()
}

func (m Model) switchRuntimeCommand(
	cfg *config.Config,
	appCfg *config.Config,
	preset modelPreset,
	notice session.Entry,
	sessionID string,
	preserveSession bool,
) tea.Cmd {
	if m.Model.Switcher == nil {
		if err := config.SaveActivePreset(preset.String()); err != nil {
			return persistErrorCmd("save active preset", err)
		}
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
	appCfgCopy := cfgCopy
	if appCfg != nil {
		appCfgCopy = *appCfg
	}

	return func() tea.Msg {
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, switchID)
		if err != nil {
			return localErrorMsg{err: err}
		}
		if err := config.SaveActivePreset(preset.String()); err != nil {
			closeSwitchedRuntime(sess, storageSess)
			return localErrorMsg{err: fmt.Errorf("save active preset: %w", err)}
		}
		if oldSession != nil {
			_ = oldSession.Close()
		}
		return runtimeSwitchedMsg{
			cfg:        &appCfgCopy,
			reasoning:  cfgCopy.ReasoningEffort,
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
			return localErrorMsg{err: err}
		}
		var entries []session.Entry
		resumeBranch := currentBranchName(m.App.Branch, storageSess)
		if storageSess != nil {
			entries, err = storageSess.Entries(context.Background())
			if err != nil {
				closeSwitchedRuntime(sess, storageSess)
				return localErrorMsg{err: fmt.Errorf("load session transcript: %w", err)}
			}
		}
		if oldSession != nil {
			_ = oldSession.Close()
		}
		printLines := []string{m.runtimeHeaderLine(backend)}
		if header := m.headerLineFor(resumeBranch); header != "" {
			printLines = append(printLines, header)
		}
		printLines = append(printLines, "", "--- resumed ---", "")
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

func closeSwitchedRuntime(sess session.AgentSession, storageSess storage.Session) {
	if sess != nil {
		_ = sess.Close()
	}
	if storageSess != nil {
		_ = storageSess.Close()
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
