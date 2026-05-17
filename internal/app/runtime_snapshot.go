package app

import (
	"context"
	"fmt"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
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
	status        string
}

type providerSelection struct {
	cfg                  *config.Config
	supportsModelListing bool
	transition           runtimeTransition
	setup                setupPromptKind
}

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

func (t runtimeTransition) persist() error {
	if t.persistState {
		if err := config.SaveState(&t.snapshot.appConfig); err != nil {
			return fmt.Errorf("save state: %w", err)
		}
	}
	if t.persistReasoning {
		if err := config.SaveReasoningState(
			t.persistReasoningSlot.String(),
			t.persistReasoningText,
		); err != nil {
			return fmt.Errorf("save state: %w", err)
		}
	}
	if t.persistActivePreset {
		if err := config.SaveActivePreset(t.snapshot.preset.String()); err != nil {
			return fmt.Errorf("save active preset: %w", err)
		}
	}
	return nil
}

func (m Model) commitRuntimeTransition(t runtimeTransition) (Model, error) {
	if err := t.persist(); err != nil {
		return m, err
	}
	m.applyRuntimeSnapshot(t.snapshot)
	return m, nil
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
	if err := ensureProviderReadyForSelection(ctx, updated); err != nil {
		def, _ := providers.Lookup(updated.Provider)
		if def.ID == providers.OpenAICompatibleID {
			return providerSelection{cfg: updated, setup: setupPromptEndpoint}, nil
		}
		return providerSelection{cfg: updated}, err
	}
	def, _ := providers.Lookup(updated.Provider)
	if providers.RequiresAuth(updated, def) && providers.ResolvedAuthToken(updated, def) == "" {
		return providerSelection{cfg: updated, setup: setupPromptAPIKey}, nil
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
	m.App.ActivePreset = snapshot.preset
	m.Progress.ReasoningEffort = normalizeThinkingValue(backendCfg.ReasoningEffort)
	if snapshot.status != "" {
		m.Progress.Status = snapshot.status
	}
}
