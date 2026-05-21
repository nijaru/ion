package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type runtimeTransition struct {
	snapshot             runtimeSnapshot
	persistState         bool
	persistReasoning     bool
	persistActivePreset  bool
	persistReasoningSlot modelPreset
	persistReasoningText string
}

type runtimeSnapshot struct {
	appConfig     config.Config
	backendConfig config.Config
	preset        modelPreset
	provider      string
	model         string
	reasoning     string
	sessionID     string
	materialized  bool
	status        string
}

type providerSelection struct {
	cfg                  *config.Config
	supportsModelListing bool
	transition           runtimeTransition
	setup                setupPromptKind
}

var saveRuntimeState = config.SaveRuntimeState

func newRuntimeSnapshot(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset modelPreset,
	status string,
) runtimeSnapshot {
	if appCfg == nil {
		appCfg = backendCfg
	}

	var appCopy config.Config
	if appCfg != nil {
		appCopy = *appCfg
	}

	backendCopy := appCopy
	if backendCfg != nil {
		backendCopy = *backendCfg
	}

	if preset == "" {
		preset = presetPrimary
	}

	return runtimeSnapshot{
		appConfig:     appCopy,
		backendConfig: backendCopy,
		preset:        preset,
		provider:      strings.TrimSpace(backendCopy.Provider),
		model:         strings.TrimSpace(backendCopy.Model),
		reasoning:     normalizeThinkingValue(backendCopy.ReasoningEffort),
		status:        status,
	}
}

func newRuntimeTransition(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset modelPreset,
	status string,
) runtimeTransition {
	return runtimeTransition{
		snapshot: newRuntimeSnapshot(appCfg, backendCfg, preset, status),
	}
}

func (t runtimeTransition) withStatus(status string) runtimeTransition {
	t.snapshot.status = status
	return t
}

func (t runtimeTransition) withRuntimeHandles(handles runtimeHandles) runtimeTransition {
	t.snapshot = t.snapshot.withRuntimeHandles(handles)
	return t
}

func (t runtimeTransition) withStatePersistence() runtimeTransition {
	t.persistState = true
	return t
}

func (t runtimeTransition) withReasoningPersistence(
	preset modelPreset,
	effort string,
) runtimeTransition {
	t.persistReasoning = true
	t.persistReasoningSlot = preset
	t.persistReasoningText = effort
	return t
}

func (t runtimeTransition) withActivePresetPersistence() runtimeTransition {
	t.persistActivePreset = true
	return t
}

func (t runtimeTransition) needsPersistence() bool {
	return t.persistState || t.persistReasoning || t.persistActivePreset
}

func (t runtimeTransition) persist() error {
	if !t.needsPersistence() {
		return nil
	}
	update := config.RuntimeStateUpdate{
		Config:              &t.snapshot.appConfig,
		PersistConfig:       t.persistState,
		ActivePreset:        t.snapshot.preset.String(),
		PersistActivePreset: t.persistActivePreset,
		ReasoningPreset:     t.persistReasoningSlot.String(),
		ReasoningEffort:     t.persistReasoningText,
		PersistReasoning:    t.persistReasoning,
	}
	if err := saveRuntimeState(update); err != nil {
		return fmt.Errorf("save state: %w", err)
	}
	return nil
}

func (m Model) commitRuntimeTransition(t runtimeTransition) (Model, error) {
	if t.needsPersistence() {
		return m, fmt.Errorf("runtime transition requires asynchronous persistence")
	}
	t = t.withRuntimeHandles(m.runtimeHandles())
	m.applyRuntimeSnapshot(t.snapshot)
	return m, nil
}

func (m Model) beginRuntimeTransitionCommit(
	t runtimeTransition,
	notice session.Entry,
) (Model, tea.Cmd) {
	if !t.needsPersistence() {
		var err error
		m, err = m.commitRuntimeTransition(t)
		if err != nil {
			return m, runtimeTransitionErrorCmd(err)
		}
		return m, m.printEntries(notice)
	}
	m.Model.RuntimeSwitchRequest++
	switchID := m.Model.RuntimeSwitchRequest
	m.Progress.Status = "Saving runtime settings..."
	return m, func() tea.Msg {
		if err := t.persist(); err != nil {
			return runtimeTransitionCommittedMsg{switchID: switchID, err: err}
		}
		return runtimeTransitionCommittedMsg{
			switchID:   switchID,
			transition: t,
			notice:     notice,
		}
	}
}

func (m Model) handleRuntimeTransitionCommitted(
	msg runtimeTransitionCommittedMsg,
) (Model, tea.Cmd) {
	if msg.switchID != 0 && msg.switchID != m.Model.RuntimeSwitchRequest {
		return m, nil
	}
	m.Model.RuntimeSwitchRequest = 0
	if isLocalBusyStatus(m.Progress.Status) {
		m.Progress.Status = ""
	}
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	m.applyRuntimeSnapshot(msg.transition.snapshot)
	m.clearProgressError()
	return m, m.printEntries(msg.notice)
}

func (m Model) providerSelection(
	ctx context.Context,
	cfg *config.Config,
	provider string,
	preset modelPreset,
) (providerSelection, error) {
	updated, err := updateProviderSelection(cfg, provider)
	if err != nil {
		return providerSelection{}, err
	}
	return providerSelectionForConfig(ctx, updated, preset)
}

func providerSelectionForConfig(
	ctx context.Context,
	updated *config.Config,
	preset modelPreset,
) (providerSelection, error) {
	setup, err := providerSetupPrompt(ctx, updated)
	if err != nil {
		return providerSelection{cfg: updated}, err
	}
	if setup != 0 {
		return providerSelection{cfg: updated, setup: setup}, nil
	}
	selection := providerSelection{
		cfg:                  updated,
		supportsModelListing: providers.SupportsModelListing(updated),
	}
	if !selection.supportsModelListing {
		selection.transition = newRuntimeTransition(
			updated,
			updated,
			preset,
			noModelConfiguredStatus(),
		).withStatePersistence().withActivePresetPersistence()
	}
	return selection, nil
}

func providerSetupPrompt(ctx context.Context, cfg *config.Config) (setupPromptKind, error) {
	if cfg == nil || strings.TrimSpace(cfg.Provider) == "" {
		return 0, nil
	}
	def, ok := providers.Lookup(cfg.Provider)
	if !ok {
		return 0, fmt.Errorf("unsupported provider %q", strings.TrimSpace(cfg.Provider))
	}
	missingAuth := providers.RequiresAuth(cfg, def) &&
		providers.ResolvedAuthToken(cfg, def) == ""
	if def.ID == providers.OpenAICompatibleID {
		if missingAuth && strings.TrimSpace(cfg.Endpoint) != "" {
			return setupPromptAPIKey, nil
		}
		if err := ensureProviderReadyForSelection(ctx, cfg); err != nil {
			return setupPromptEndpoint, nil
		}
		if missingAuth {
			return setupPromptAPIKey, nil
		}
		return 0, nil
	}
	if missingAuth {
		return setupPromptAPIKey, nil
	}
	return 0, nil
}

func (m Model) modelSelectionTransition(
	cfg *config.Config,
	preset modelPreset,
	model string,
) (runtimeTransition, *config.Config, error) {
	updated := updateModelForPreset(cfg, model, preset)
	runtimeCfg, err := m.runtimeConfigForPreset(updated, preset)
	if err != nil {
		return runtimeTransition{}, nil, err
	}
	transition := newRuntimeTransition(updated, runtimeCfg, preset, "").
		withStatePersistence()
	return transition, runtimeCfg, nil
}

func (m Model) thinkingSelectionTransition(
	cfg *config.Config,
	preset modelPreset,
	level string,
) (runtimeTransition, *config.Config, error) {
	updated := updateThinkingForPreset(cfg, level, preset)
	runtimeCfg, err := m.runtimeConfigForPreset(updated, preset)
	if err != nil {
		return runtimeTransition{}, nil, err
	}
	transition := newRuntimeTransition(updated, runtimeCfg, preset, "").
		withReasoningPersistence(preset, level)
	return transition, runtimeCfg, nil
}

func resumeSelectionTransition(cfg *config.Config) runtimeTransition {
	return newRuntimeTransition(
		cfg,
		cfg,
		presetPrimary,
		"",
	).withActivePresetPersistence()
}

func runtimeTransitionErrorCmd(err error) tea.Cmd {
	if err == nil {
		return nil
	}
	return func() tea.Msg {
		return localErrorMsg{err: err}
	}
}

func (m *Model) applyRuntimeSnapshot(snapshot runtimeSnapshot) {
	appCfg := snapshot.appConfig
	backendCfg := snapshot.backendConfig

	if m.Model.Backend != nil {
		m.Model.Backend.SetConfig(&backendCfg)
	}
	m.Model.Config = &appCfg
	m.Model.Runtime = snapshot
	m.App.ActivePreset = snapshot.preset
	m.Progress.ReasoningEffort = snapshot.reasoning
	if snapshot.status != "" {
		m.Progress.Status = snapshot.status
	}
}

func newAcceptedRuntime(
	transition runtimeTransition,
	handles runtimeHandles,
) acceptedRuntime {
	return acceptedRuntime{
		transition: transition.withRuntimeHandles(handles),
		handles:    handles,
	}
}

func (m Model) runtimeHandles() runtimeHandles {
	return runtimeHandles{
		backend: m.Model.Backend,
		session: m.Model.Session,
		storage: m.Model.Storage,
	}
}

func (s runtimeSnapshot) withRuntimeHandles(handles runtimeHandles) runtimeSnapshot {
	if s.provider == "" && handles.backend != nil {
		s.provider = strings.TrimSpace(handles.backend.Provider())
	}
	if s.model == "" && handles.backend != nil {
		s.model = strings.TrimSpace(handles.backend.Model())
	}
	if s.reasoning == "" {
		s.reasoning = config.DefaultReasoningEffort
	}
	s.sessionID, s.materialized = runtimeSessionState(handles)
	return s
}

func runtimeSessionState(handles runtimeHandles) (string, bool) {
	if handles.storage != nil {
		id := strings.TrimSpace(handles.storage.ID())
		return id, storage.IsMaterialized(handles.storage)
	}
	if handles.session == nil {
		return "", false
	}
	id := strings.TrimSpace(handles.session.ID())
	return id, id != ""
}

func (s runtimeSnapshot) materializedSessionID() string {
	if !s.materialized {
		return ""
	}
	return strings.TrimSpace(s.sessionID)
}

func (m Model) runtimeProvider() string {
	if provider := strings.TrimSpace(m.Model.Runtime.provider); provider != "" {
		return provider
	}
	if m.Model.Backend == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Backend.Provider())
}

func (m Model) runtimeModel() string {
	if model := strings.TrimSpace(m.Model.Runtime.model); model != "" {
		return model
	}
	if m.Model.Backend == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Backend.Model())
}
