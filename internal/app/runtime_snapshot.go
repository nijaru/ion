package app

import (
	"github.com/nijaru/ion/internal/config"
)

type runtimeSnapshot struct {
	appConfig     config.Config
	backendConfig config.Config
	preset        modelPreset
	status        string
}

func newRuntimeSnapshot(
	appCfg *config.Config,
	backendCfg *config.Config,
	preset modelPreset,
	status string,
) runtimeSnapshot {
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
