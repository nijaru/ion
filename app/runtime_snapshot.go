package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/core"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type ProviderSelection = core.ProviderSelection

var saveRuntimeState = config.SaveRuntimeState

func newRuntimeSnapshot(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset core.Preset,
	status string,
) core.Snapshot {
	return core.NewSnapshot(appCfg, backendCfg, preset, status)
}

func newRuntimeTransition(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset core.Preset,
	status string,
) core.Transition {
	return core.NewTransition(appCfg, backendCfg, preset, status)
}

func (m Model) commitRuntimeTransition(t core.Transition) (Model, error) {
	if t.NeedsPersistence() {
		return m, fmt.Errorf("runtime transition requires asynchronous persistence")
	}
	t = t.WithHandles(m.Handles())
	m.applyRuntimeSnapshot(t.Snapshot)
	return m, nil
}

func (m Model) beginRuntimeTransitionCommit(
	t core.Transition,
	notice session.Entry,
) (Model, tea.Cmd) {
	if !t.NeedsPersistence() {
		var err error
		m, err = m.commitRuntimeTransition(t)
		if err != nil {
			return m, TransitionErrorCmd(err)
		}
		return m, m.terminalCommit().Entries(notice)
	}
	switchID := m.runtimeRequest().begin("Saving runtime settings...")
	return m, func() tea.Msg {
		if err := t.Persist(saveRuntimeState); err != nil {
			return TransitionCommittedMsg{switchID: switchID, err: err}
		}
		return TransitionCommittedMsg{
			switchID:   switchID,
			transition: t,
			notice:     notice,
		}
	}
}

func (m Model) handleRuntimeTransitionCommitted(
	msg TransitionCommittedMsg,
) (Model, tea.Cmd) {
	if !m.runtimeRequest().finish(msg.switchID) {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	transition := msg.transition.WithHandles(m.Handles())
	m.applyRuntimeSnapshot(transition.Snapshot)
	m.clearProgressError()
	return m, m.terminalCommit().Entries(msg.notice)
}

func (m Model) ProviderSelection(
	ctx context.Context,
	cfg *config.Config,
	provider string,
	preset core.Preset,
) (core.ProviderSelection, error) {
	updated, err := updateProviderSelection(cfg, provider)
	if err != nil {
		return core.ProviderSelection{}, err
	}
	return providerSelectionForConfig(ctx, updated, preset)
}

func providerSelectionForConfig(
	ctx context.Context,
	updated *config.Config,
	preset core.Preset,
) (core.ProviderSelection, error) {
	setup, err := providerSetupPrompt(ctx, updated)
	if err != nil {
		return core.ProviderSelection{Config: updated}, err
	}
	if setup != 0 {
		return core.ProviderSelection{Config: updated, Setup: setup}, nil
	}
	selection := core.ProviderSelection{
		Config:               updated,
		SupportsModelListing: llm.SupportsModelListing(updated),
	}
	if !selection.SupportsModelListing {
		selection.Transition = newRuntimeTransition(
			updated,
			updated,
			preset,
			noModelConfiguredStatus(),
		).WithStatePersistence().WithActivePresetPersistence()
	}
	return selection, nil
}

func providerSetupPrompt(ctx context.Context, cfg *config.Config) (core.SetupPromptKind, error) {
	if cfg == nil || strings.TrimSpace(cfg.Provider) == "" {
		return 0, nil
	}
	def, ok := llm.Lookup(cfg.Provider)
	if !ok {
		return 0, fmt.Errorf("unsupported provider %q", strings.TrimSpace(cfg.Provider))
	}
	missingAuth := llm.RequiresAuth(cfg, def) &&
		llm.ResolvedAuthToken(cfg, def) == ""
	if def.ID == llm.OpenAICompatibleID {
		if missingAuth && strings.TrimSpace(cfg.Endpoint) != "" {
			return core.SetupPromptAPIKey, nil
		}
		if err := ensureProviderReadyForSelection(ctx, cfg); err != nil {
			return core.SetupPromptEndpoint, nil
		}
		if missingAuth {
			return core.SetupPromptAPIKey, nil
		}
		return 0, nil
	}
	if missingAuth {
		return core.SetupPromptAPIKey, nil
	}
	return 0, nil
}

func (m Model) modelSelectionTransition(
	cfg *config.Config,
	preset core.Preset,
	model string,
) (core.Transition, *config.Config, error) {
	updated := updateModelForPreset(cfg, model, preset)
	runtimeCfg, err := m.runtimeConfigForPreset(updated, preset)
	if err != nil {
		return core.Transition{}, nil, err
	}
	transition := newRuntimeTransition(updated, runtimeCfg, preset, "").
		WithStatePersistence()
	return transition, runtimeCfg, nil
}

func (m Model) thinkingSelectionTransition(
	cfg *config.Config,
	preset core.Preset,
	level string,
) (core.Transition, *config.Config, error) {
	updated := updateThinkingForPreset(cfg, level, preset)
	runtimeCfg, err := m.runtimeConfigForPreset(updated, preset)
	if err != nil {
		return core.Transition{}, nil, err
	}
	transition := newRuntimeTransition(updated, runtimeCfg, preset, "").
		WithReasoningPersistence(preset, level)
	return transition, runtimeCfg, nil
}

func resumeSelectionTransition(cfg *config.Config) core.Transition {
	return newRuntimeTransition(
		cfg,
		cfg,
		presetPrimary,
		"",
	).WithActivePresetPersistence()
}

func TransitionErrorCmd(err error) tea.Cmd {
	if err == nil {
		return nil
	}
	return func() tea.Msg {
		return localErrorMsg{err: err}
	}
}

func (m *Model) applyRuntimeSnapshot(snapshot core.Snapshot) {
	appCfg := snapshot.AppConfig
	backendCfg := snapshot.BackendConfig

	if m.Model.Backend != nil {
		m.Model.Backend.SetConfig(&backendCfg)
	}
	m.Model.Config = &appCfg
	m.Model.Runtime = snapshot
	m.App.ActivePreset = snapshot.Preset
	m.progressReducer().applyRuntimeSnapshot(snapshot)
}

func (m *Model) refreshRuntimeSessionSnapshot() {
	sessionID, materialized := core.SessionState(m.Handles())
	m.Model.Runtime.SessionID = sessionID
	m.Model.Runtime.Materialized = materialized
}

func newAcceptedRuntime(
	transition core.Transition,
	handles core.Handles,
) core.Accepted {
	return core.NewAccepted(transition, handles)
}

func (m Model) Handles() core.Handles {
	return core.Handles{
		Backend: m.Model.Backend,
		Session: m.Model.Session,
		Storage: m.Model.Storage,
	}
}

func (m Model) runtimeProvider() string {
	if provider := strings.TrimSpace(m.Model.Runtime.Provider); provider != "" {
		return provider
	}
	if m.Model.Backend == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Backend.Provider())
}

func (m Model) runtimeModel() string {
	if model := strings.TrimSpace(m.Model.Runtime.Model); model != "" {
		return model
	}
	if m.Model.Backend == nil {
		return ""
	}
	return strings.TrimSpace(m.Model.Backend.Model())
}
