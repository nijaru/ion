package app

import (
	"context"
	"fmt"
	"github.com/nijaru/ion/config"
	"os"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func (m Model) resumeStoredSessionByID(sessionID string) (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("session store not available")
	}

	switchID := m.runtimeRequest().begin("Loading session...")
	store := m.Model.Store
	return m, func() tea.Msg {
		cfg, err := m.storedSessionConfig(context.Background(), store, sessionID)
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: switchID, err: err}
		}
		return resumeSessionSelectedMsg{
			switchID:  switchID,
			sessionID: sessionID,
			cfg:       cfg,
		}
	}
}

func (m Model) storedSessionConfig(
	ctx context.Context,
	store session.Store,
	sessionID string,
) (*config.Config, error) {
	if _, err := store.GetEntry(ctx, sessionID); err != nil {
		return nil, fmt.Errorf("failed to find session %s: %w", sessionID, err)
	}
	var model string
	if lookup, ok := store.(sessionCatalogLookup); ok {
		info, err := lookup.GetSessionInfo(ctx, sessionID)
		if err == nil {
			model = info.Model
		}
	}
	if model == "" {
		catalog, ok := store.(sessionCatalogReader)
		if !ok {
			return nil, fmt.Errorf("session store does not support session catalog")
		}
		sessions, err := catalog.ListSessions(ctx, m.App.Workdir)
		if err != nil {
			return nil, fmt.Errorf("failed to list sessions: %w", err)
		}
		for _, info := range sessions {
			if info.ID() == sessionID {
				model = info.Model
				break
			}
		}
	}
	provider, modelName := splitStoredSessionModel(model)
	if provider == "" || modelName == "" {
		return nil, fmt.Errorf("session %s is missing provider/model metadata", sessionID)
	}
	cfg, err := m.configForStoredSession(provider, modelName)
	if err != nil {
		return nil, fmt.Errorf("failed to apply session metadata: %w", err)
	}
	return cfg, nil
}

func (m Model) handleResumeSessionSelected(msg resumeSessionSelectedMsg) (Model, tea.Cmd) {
	if !m.runtimeRequest().matches(msg.switchID) {
		return m, nil
	}
	notice := systemEntry("Resumed session " + msg.sessionID)
	return m.resumeRuntimeCommand(msg.cfg, notice, msg.sessionID)
}

func (m Model) configForStoredSession(provider, model string) (*config.Config, error) {
	cfg, err := m.commandConfig()
	if err != nil {
		return nil, fmt.Errorf("load config: %w", err)
	}
	cfg, err = updateProviderSelection(cfg, provider)
	if err != nil {
		return nil, err
	}
	return updateModelForPreset(cfg, model, PresetPrimary), nil
}

func (m Model) switchPresetCommand(preset Preset) (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage("changing presets"))
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForPreset(cfg, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve %s preset: %v", preset, err))
	}
	notice := systemEntry("Switched to " + preset.String())
	transition := newRuntimeTransition(cfg, runtimeCfg, preset, "")
	return m.switchRuntimeCommand(
		transition,
		notice,
		m.currentMaterializedSessionID(),
		false,
	)
}

func (m Model) cycleScopedModelCommand(forward bool) (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage("changing models"))
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	preset := m.activePreset()
	runtimeCfg, err := m.runtimeConfigForPreset(cfg, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve %s preset: %v", preset, err))
	}
	next, ok := pickScopedModel(resolveScopedModelPatterns(context.Background(), cfg), runtimeCfg.Provider, runtimeCfg.Model, forward)
	if !ok {
		return m.cyclePresetFallback(cfg, preset, forward)
	}
	updated := *cfg
	updated.Provider = next.Provider
	switch preset {
	case PresetFast:
		updated.FastModel = next.Model
		if next.Thinking != "" {
			updated.FastReasoningEffort = next.Thinking
		}
	default:
		updated.Model = next.Model
		if next.Thinking != "" {
			updated.ReasoningEffort = next.Thinking
		}
	}
	runtimeUpdated, err := m.runtimeConfigForPreset(&updated, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve scoped model: %v", err))
	}
	label := fmt.Sprintf("%s/%s", next.Provider, next.Model)
	notice := systemEntry("Switched to " + label)
	transition := newRuntimeTransition(&updated, runtimeUpdated, preset, "")
	return m.switchRuntimeCommand(
		transition,
		notice,
		m.currentMaterializedSessionID(),
		false,
	)
}

func (m Model) cyclePresetFallback(
	cfg *config.Config,
	preset Preset,
	forward bool,
) (Model, tea.Cmd) {
	// Build available models list: primary + fast (if configured and different)
	available := m.buildAvailableModels(cfg)
	if len(available) <= 1 {
		// Only one model available — open picker to configure fast
		return m.openModelPickerForPreset(cfg, PresetFast)
	}

	// Find current model in available list
	currentProvider := cfg.Provider
	var currentModel string
	switch preset {
	case PresetFast:
		currentModel = cfg.FastModel
	default:
		currentModel = cfg.Model
	}

	currentIndex := -1
	for i, am := range available {
		if am.Provider == currentProvider && am.Model == currentModel {
			currentIndex = i
			break
		}
	}
	if currentIndex == -1 {
		currentIndex = 0
	}

	// Cycle
	length := len(available)
	var nextIndex int
	if forward {
		nextIndex = (currentIndex + 1) % length
	} else {
		nextIndex = (currentIndex - 1 + length) % length
	}
	next := available[nextIndex]

	// Apply model change
	updated := *cfg
	updated.Provider = next.Provider
	switch preset {
	case PresetFast:
		updated.FastModel = next.Model
	default:
		updated.Model = next.Model
	}

	runtimeUpdated, err := m.runtimeConfigForPreset(&updated, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve model: %v", err))
	}
	label := fmt.Sprintf("%s/%s", next.Provider, next.Model)
	notice := systemEntry("Switched to " + label)
	transition := newRuntimeTransition(&updated, runtimeUpdated, preset, "")
	return m.switchRuntimeCommand(
		transition,
		notice,
		m.currentMaterializedSessionID(),
		false,
	)
}

// availableModel represents a provider/model pair for cycling.
type availableModel struct {
	Provider string
	Model    string
}

// buildAvailableModels builds the list of available provider/model pairs.
// Includes primary model and fast model (if configured and different).
// Uses originalPrimaryModel to preserve the full list after cycling.
func (m Model) buildAvailableModels(cfg *config.Config) []availableModel {
	var available []availableModel

	// Use original primary model if available, otherwise fall back to config
	primaryModel := m.Model.originalPrimaryModel
	if primaryModel == "" {
		primaryModel = cfg.Model
	}

	// Primary model
	if cfg.Provider != "" && primaryModel != "" {
		available = append(available, availableModel{
			Provider: cfg.Provider,
			Model:    primaryModel,
		})
	}

	// Fast model (if different from primary)
	if cfg.FastModel != "" && cfg.FastModel != primaryModel {
		available = append(available, availableModel{
			Provider: cfg.Provider,
			Model:    cfg.FastModel,
		})
	}

	return available
}

// filterScopedModelsByAuth returns only scoped models with configured API keys.
// Models with empty provider are included (they use the default provider).
// If no credentials file exists, all models are included (allows tests to work).
func filterScopedModelsByAuth(models []config.ScopedModel) []config.ScopedModel {
	// If no credentials file exists, don't filter (test environment)
	credPath, err := config.CredentialPath()
	if err != nil {
		return models
	}
	if _, err := os.Stat(credPath); os.IsNotExist(err) {
		return models
	}

	var filtered []config.ScopedModel
	for _, sm := range models {
		if sm.Provider == "" || config.HasAPIKey(sm.Provider) {
			filtered = append(filtered, sm)
		}
	}
	return filtered
}

// resolveScopedModelPatterns expands glob patterns in scoped models against available models.
// Patterns without glob characters are passed through as-is.
func resolveScopedModelPatterns(ctx context.Context, cfg *config.Config) []config.ScopedModel {
	if cfg == nil || len(cfg.ScopedModels) == 0 {
		return nil
	}

	var result []config.ScopedModel
	for _, sm := range cfg.ScopedModels {
		if sm.Pattern == "" {
			// Exact model — pass through
			result = append(result, sm)
			continue
		}

		// Glob pattern — resolve against available models
		matched := matchModelsByPattern(ctx, cfg, sm.Pattern)
		for _, m := range matched {
			result = append(result, config.ScopedModel{
				Provider: m.Provider,
				Model:    m.ID,
				Thinking: sm.Thinking,
			})
		}
	}
	return result
}

// matchModelsByPattern returns models matching a glob pattern.
// Matches against "provider/model" or just "model".
func matchModelsByPattern(ctx context.Context, cfg *config.Config, pattern string) []llm.ModelMetadata {
	models, err := listModelsForConfig(ctx, cfg)
	if err != nil {
		return nil
	}

	pattern = strings.ToLower(pattern)
	var matched []llm.ModelMetadata
	for _, m := range models {
		fullID := strings.ToLower(m.Provider + "/" + m.ID)
		modelID := strings.ToLower(m.ID)
		if matchGlob(pattern, fullID) || matchGlob(pattern, modelID) {
			matched = append(matched, m)
		}
	}
	return matched
}

// matchGlob does simple glob matching (* and ?).
func matchGlob(pattern, name string) bool {
	matched, err := filepath.Match(pattern, name)
	return err == nil && matched
}

func pickScopedModel(
	models []config.ScopedModel,
	currentProvider string,
	currentModel string,
	forward bool,
) (config.ScopedModel, bool) {
	// Filter by auth availability (Pi parity)
	models = filterScopedModelsByAuth(models)
	if len(models) <= 1 {
		return config.ScopedModel{}, false
	}
	currentProvider = strings.ToLower(strings.TrimSpace(currentProvider))
	currentModel = strings.TrimSpace(currentModel)
	idx := -1
	for i, sm := range models {
		if strings.EqualFold(sm.Provider, currentProvider) &&
			strings.TrimSpace(sm.Model) == currentModel {
			idx = i
			break
		}
	}
	if idx == -1 {
		idx = 0
	}
	n := len(models)
	if forward {
		idx = (idx + 1) % n
	} else {
		idx = (idx - 1 + n) % n
	}
	return models[idx], true
}

func (m Model) currentMaterializedSessionID() string {
	id := ""
	if m.Model.Runtime.Materialized {
		id = m.Model.Runtime.SessionID
	}
	if id != "" {
		return id
	}
	if m.activeSession() == nil {
		return ""
	}
	if m.Model.Storage == nil {
		return m.activeSession().ID()
	}
	return strings.TrimSpace(m.Model.Storage.ID())
}

func (m Model) ResumeSessionID() string {
	return m.currentMaterializedSessionID()
}

func (m Model) switchRuntimeCommand(
	transition Transition,
	notice session.Entry,
	sessionID string,
	preserveSession bool,
) (Model, tea.Cmd) {
	transition = transition.WithActivePresetPersistence(m.App.ActivePreset)

	if m.Model.Switcher == nil {
		return m.beginRuntimeTransitionCommit(transition, notice)
	}

	switcher := m.Model.Switcher
	current := m.Handles()
	requestID := m.runtimeRequest().begin("Switching runtime...")

	return m, func() tea.Msg {
		result, err := Switch(context.Background(), SwitchInput{
			Switcher:        switcher,
			Transition:      transition,
			Current:         current,
			TargetSessionID: sessionID,
			PreserveSession: preserveSession,
			SaveState:       saveRuntimeState,
		})
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: requestID, err: err}
		}
		return runtimeSwitchedMsg{
			switchID:   requestID,
			runtime:    result.Runtime,
			previous:   result.Previous,
			notice:     session.EntryText(notice),
			showStatus: preserveSession,
		}
	}
}

func (m Model) resumeRuntimeCommand(
	cfg *config.Config,
	notice session.Entry,
	sessionID string,
) (Model, tea.Cmd) {
	transition := resumeSelectionTransition(cfg)

	if m.Model.Switcher == nil {
		return m.beginRuntimeTransitionCommit(transition, notice)
	}
	switcher := m.Model.Switcher
	current := m.Handles()
	switchID := m.runtimeRequest().begin("Switching runtime...")
	return m, func() tea.Msg {
		result, err := Resume(context.Background(), ResumeInput{
			Switcher:   switcher,
			Transition: transition,
			Current:    current,
			SessionID:  sessionID,
			SaveState:  saveRuntimeState,
		})
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: switchID, err: err}
		}
		resumeBranch := currentBranchName(m.App.Branch, result.Runtime.Handles.Storage)
		printLines := []string{m.runtimeHeaderLine(result.Runtime.Handles.Backend)}
		if header := m.headerLineFor(resumeBranch); header != "" {
			printLines = append(printLines, header)
		}
		printLines = append(printLines, "", "--- resumed ---", "")
		return runtimeSwitchedMsg{
			switchID:      switchID,
			runtime:       result.Runtime,
			previous:      result.Previous,
			printLines:    printLines,
			replayEntries: func() []session.Entry { e, _ := result.GetEntries(context.Background(), m.Model.Store); return e }(),
			notice:        session.EntryText(notice),
			showStatus:    false,
		}
	}
}

func (m Model) handleRuntimeSwitched(msg runtimeSwitchedMsg) (Model, tea.Cmd) {
	if !m.runtimeRequest().matches(msg.switchID) {
		closeRuntimeHandles(msg.runtime.Handles)
		return m, nil
	}

	m.applyRuntimeSwitched(msg)
	cmds := m.runtimeSwitchedCommands(msg)
	return m, tea.Sequence(cmds...)
}

func (m *Model) applyRuntimeSwitched(msg runtimeSwitchedMsg) {
	// Replacement is live before the previous runner is closed. This is the
	// after-switch lifecycle boundary; Switch/Resume deliberately do not run
	// teardown on construction failure.
	m.runtimeRequest().clear()
	m.Model.Backend = msg.runtime.Handles.Backend
	m.Model.Runner = msg.runtime.Handles.Runner
	m.Model.Storage = msg.runtime.Handles.Storage
	m.applyRuntimeSnapshot(msg.runtime.Transition.Snapshot)
	closeRuntimeHandles(msg.previous)
	m.Model.EventGeneration++
	m.pickerReducer().closeAll()
	m.clearProgressError()
	if msg.runtime.Handles.Storage != nil {
		meta := msg.runtime.Handles.Storage.Meta()
		m.App.Branch = meta.Branch
	}
	m.turnReducer().ClearActiveState(true)
	m.progressReducer().clearLocalBusyStatus()
	m.progressReducer().markRuntimeReady()
	m.turnReducer().ResetFinishedTurnSummary()
	m.clearPendingAction()
	m.progressReducer().resetSessionUsage()
	m.resetHistoryCursor()
}

func (m *Model) runtimeSwitchedCommands(msg runtimeSwitchedMsg) []tea.Cmd {
	cmds := make([]tea.Cmd, 0, 3)

	var status string
	s := msg.runtime.Transition.Snapshot.Status
	if msg.showStatus && strings.TrimSpace(s) != "" && !isConfigurationStatus(s) {
		status = s
	}

	if cmd := m.terminalCommit().SwitchReplay(msg.printLines, msg.replayEntries, msg.notice, status); cmd != nil {
		cmds = append(cmds, cmd)
	}

	if msg.runtime.Handles.Storage != nil {
		cmds = append(
			cmds,
			loadSessionUsageCmd(m.Model.EventGeneration, msg.runtime.Handles.Storage),
		)
	}
	return append(cmds, m.awaitSessionEvent())
}

func (m Model) handleRuntimeSwitchError(msg runtimeSwitchErrorMsg) (Model, tea.Cmd) {
	if !m.runtimeRequest().matches(msg.switchID) {
		return m, nil
	}
	m.runtimeRequest().clear()
	return m.handleLocalError(msg.err)
}

func closeRuntimeHandles(handles Handles) {
	CloseHandles(handles)
}

func currentBranchName(defaultBranch string, sess persistenceAdapter) string {
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
func (m Model) activePreset() Preset {
	switch m.App.ActivePreset {
	case PresetFast:
		return PresetFast
	default:
		return PresetPrimary
	}
}

func (m Model) activePresetTitle() string {
	return presetTitle(m.activePreset())
}

func presetTitle(preset Preset) string {
	switch preset {
	case PresetFast:
		return "fast"
	default:
		return "primary"
	}
}

func (m Model) runtimeConfigForPreset(
	cfg *config.Config,
	preset Preset,
) (*config.Config, error) {
	return llm.ResolveRuntimeConfig(context.Background(), cfg, llm.Preset(preset))
}

func (m Model) runtimeConfigForActivePreset(cfg *config.Config) (*config.Config, error) {
	return m.runtimeConfigForPreset(cfg, m.activePreset())
}

func (m Model) commandConfig() (*config.Config, error) {
	if m.Model.Config != nil {
		copied := *m.Model.Config
		return &copied, nil
	}
	return &config.Config{}, nil
}

func mergeRuntimeSelection(dst, runtime *config.Config) {
	if dst == nil || runtime == nil {
		return
	}
	if strings.TrimSpace(runtime.Provider) != "" {
		dst.Provider = runtime.Provider
		dst.Model = runtime.Model
	}
	if strings.TrimSpace(runtime.ReasoningEffort) != "" {
		dst.ReasoningEffort = runtime.ReasoningEffort
	}
	if strings.TrimSpace(runtime.FastModel) != "" {
		dst.FastModel = runtime.FastModel
	}
	if strings.TrimSpace(runtime.FastReasoningEffort) != "" {
		dst.FastReasoningEffort = runtime.FastReasoningEffort
	}
	if strings.TrimSpace(runtime.SummaryModel) != "" {
		dst.SummaryModel = runtime.SummaryModel
	}
	if strings.TrimSpace(runtime.SummaryReasoningEffort) != "" {
		dst.SummaryReasoningEffort = runtime.SummaryReasoningEffort
	}
}

func updateProviderSelection(
	cfg *config.Config,
	provider string,
) (*config.Config, error) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	resolved := llm.ResolveID(provider)
	def, ok := llm.Lookup(resolved)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", strings.TrimSpace(provider))
	}
	if def.Runtime != llm.RuntimeNative {
		return nil, fmt.Errorf("ACP providers are deferred until the advanced integration phase")
	}
	updated := *cfg
	updated.Provider = def.ID
	if llm.ResolveID(cfg.Provider) == def.ID {
		return &updated, nil
	}
	updated.Model = ""
	updated.FastModel = ""
	updated.FastReasoningEffort = ""
	updated.SummaryModel = ""
	updated.SummaryReasoningEffort = ""
	return &updated, nil
}

func updateModelForPreset(
	cfg *config.Config,
	model string,
	preset Preset,
) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	model = strings.TrimSpace(model)
	switch preset {
	case PresetFast:
		updated.FastModel = model
	default:
		updated.Model = model
	}
	return &updated
}

func updateThinkingForPreset(
	cfg *config.Config,
	effort string,
	preset Preset,
) *config.Config {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	effort = strings.TrimSpace(effort)
	switch preset {
	case PresetFast:
		updated.FastReasoningEffort = effort
	default:
		updated.ReasoningEffort = effort
	}
	return &updated
}

func configuredModelForPreset(cfg *config.Config, preset Preset) string {
	if cfg == nil {
		return ""
	}
	switch preset {
	case PresetFast:
		return strings.TrimSpace(cfg.FastModel)
	default:
		return strings.TrimSpace(cfg.Model)
	}
}
