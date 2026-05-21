package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/runtimecontroller"
	"github.com/nijaru/ion/internal/session"
)

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
	return runtimecontroller.NewSnapshot(appCfg, backendCfg, preset, status)
}

func newRuntimeTransition(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset modelPreset,
	status string,
) runtimeTransition {
	return runtimecontroller.NewTransition(appCfg, backendCfg, preset, status)
}

func (m Model) commitRuntimeTransition(t runtimeTransition) (Model, error) {
	if t.NeedsPersistence() {
		return m, fmt.Errorf("runtime transition requires asynchronous persistence")
	}
	t = t.WithHandles(m.runtimeHandles())
	m.applyRuntimeSnapshot(t.Snapshot)
	return m, nil
}

func (m Model) beginRuntimeTransitionCommit(
	t runtimeTransition,
	notice session.Entry,
) (Model, tea.Cmd) {
	if !t.NeedsPersistence() {
		var err error
		m, err = m.commitRuntimeTransition(t)
		if err != nil {
			return m, runtimeTransitionErrorCmd(err)
		}
		return m, m.printEntries(notice)
	}
	m.Model.RuntimeSwitchRequest++
	switchID := m.Model.RuntimeSwitchRequest
	m.progressReducer().beginLocalStatus("Saving runtime settings...")
	return m, func() tea.Msg {
		if err := t.Persist(saveRuntimeState); err != nil {
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
	m.progressReducer().clearLocalBusyStatus()
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	transition := msg.transition.WithHandles(m.runtimeHandles())
	m.applyRuntimeSnapshot(transition.Snapshot)
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
		).WithStatePersistence().WithActivePresetPersistence()
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
		WithStatePersistence()
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
		WithReasoningPersistence(preset, level)
	return transition, runtimeCfg, nil
}

func resumeSelectionTransition(cfg *config.Config) runtimeTransition {
	return newRuntimeTransition(
		cfg,
		cfg,
		presetPrimary,
		"",
	).WithActivePresetPersistence()
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

func newAcceptedRuntime(
	transition runtimeTransition,
	handles runtimeHandles,
) acceptedRuntime {
	return runtimecontroller.NewAccepted(transition, handles)
}

func (m Model) runtimeHandles() runtimeHandles {
	return runtimeHandles{
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
