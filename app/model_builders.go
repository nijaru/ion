package app

import (
	"github.com/nijaru/ion/config"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

func (m Model) WithPrintedTranscript(v bool) Model {
	m.App.PrintedTranscript = v
	return m
}

func (m Model) WithSize(width, height int) Model {
	m.App.Width = width
	m.App.Height = height
	m.layout()
	return m
}

func (m Model) WithConfig(cfg *config.Config) Model {
	return m.WithConfigForRuntime(cfg, cfg)
}

func (m Model) WithConfigForRuntime(cfg, runtimeCfg *config.Config) Model {
	return m.WithConfigForRuntimePreset(cfg, runtimeCfg, m.activePreset().String())
}

func (m Model) WithConfigForRuntimePreset(
	cfg, runtimeCfg *config.Config,
	preset string,
) Model {
	if cfg == nil {
		return m
	}
	snapshot := newRuntimeSnapshot(
		cfg,
		runtimeCfg,
		PresetFromString(preset),
		"",
	).WithHandles(m.Handles())
	m.applyRuntimeSnapshot(snapshot)
	return m
}

func (m Model) WithActivePreset(value string) Model {
	m.App.ActivePreset = PresetFromString(value)
	return m
}

func (m Model) WithSessionPicker() Model {
	m, _ = m.openSessionPicker()
	return m
}

func (m Model) WithSessionPreStartupMode() Model {
	m.Picker.PreStartupMode = true
	m, _ = m.openSessionPicker()
	return m
}

func (m Model) WithProviderPicker() Model {
	m, _ = m.openProviderPicker()
	return m
}

func (m Model) WithModelPicker() Model {
	m, _ = m.openModelPicker()
	return m
}

func (m Model) WithCheckpointStore(store *ionworkspace.CheckpointStore) Model {
	m.Model.Checkpoints = store
	return m
}
