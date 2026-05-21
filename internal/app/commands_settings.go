package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

func (m Model) handleSettingsCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) == 1 {
		if m.Model.SettingsRequest != 0 {
			return m, cmdError(m.localCommandBusyMessage("loading settings"))
		}
		m.Model.SettingsRequest++
		requestID := m.Model.SettingsRequest
		m.progressReducer().beginLocalStatus("Loading settings...")
		return m, func() tea.Msg {
			cfg, err := loadStableConfig()
			if err != nil {
				return settingsCommandMsg{
					requestID: requestID,
					err:       fmt.Errorf("failed to load config: %w", err),
				}
			}
			return settingsCommandMsg{
				requestID: requestID,
				summary:   m.settingsSummary(cfg),
			}
		}
	}
	if len(fields) != 3 {
		return m, cmdError(
			"usage: /settings [retry on|off|tool auto|full|collapsed|hidden|read full|summary|hidden|write diff|summary|hidden|bash full|summary|hidden|thinking full|collapsed|hidden|busy queue|steer]",
		)
	}

	key := strings.ToLower(strings.TrimSpace(fields[1]))
	value := strings.ToLower(strings.TrimSpace(fields[2]))
	if _, _, err := settingsConfigUpdate(&config.Config{}, key, value); err != nil {
		return m, cmdError(err.Error())
	}
	m.Model.SettingsRequest++
	requestID := m.Model.SettingsRequest
	m.progressReducer().beginLocalStatus("Saving settings...")
	activeCfg := m.Model.Config
	preset := m.activePreset()
	return m, func() tea.Msg {
		stableCfg, err := loadStableConfig()
		if err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to load config: %w", err),
			}
		}
		updated, notice, err := settingsConfigUpdate(stableCfg, key, value)
		if err != nil {
			return settingsCommandMsg{requestID: requestID, err: err}
		}
		if err := saveConfigFile(&updated); err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to save config: %w", err),
			}
		}
		runtimeCfg, err := loadConfigFile()
		if err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to reload runtime config: %w", err),
			}
		}
		mergeRuntimeSelection(runtimeCfg, activeCfg)
		backendCfg, err := m.runtimeConfigForPreset(runtimeCfg, preset)
		if err != nil {
			return settingsCommandMsg{
				requestID: requestID,
				err:       fmt.Errorf("failed to resolve active preset: %w", err),
			}
		}
		return settingsCommandMsg{
			requestID:     requestID,
			transition:    newRuntimeTransition(runtimeCfg, backendCfg, preset, ""),
			hasTransition: true,
			notice:        notice,
		}
	}
}

func (m Model) handleSettingsCommandResult(msg settingsCommandMsg) (Model, tea.Cmd) {
	if msg.requestID == 0 || msg.requestID != m.Model.SettingsRequest {
		return m, nil
	}
	m.Model.SettingsRequest = 0
	m.progressReducer().clearLocalBusyStatus()
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	if msg.summary != "" {
		return m, m.printEntries(session.Entry{Role: session.System, Content: msg.summary})
	}
	if !msg.hasTransition {
		return m, nil
	}
	var err error
	m, err = m.commitRuntimeTransition(msg.transition)
	if err != nil {
		return m, runtimeTransitionErrorCmd(err)
	}
	return m, m.printEntries(session.Entry{Role: session.System, Content: msg.notice})
}

func settingsConfigUpdate(
	cfg *config.Config,
	key string,
	value string,
) (config.Config, string, error) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	updated := *cfg
	var notice string

	switch key {
	case "retry":
		enabled, ok := parseOnOff(value)
		if !ok {
			return config.Config{}, "", fmt.Errorf("usage: /settings retry on|off")
		}
		updated.RetryUntilCancelled = &enabled
		if enabled {
			notice = "Retry network errors: on"
		} else {
			notice = "Retry network errors: off"
		}
	case "tool", "tools":
		verbosity, ok := parseToolVerbosity(value)
		if !ok {
			return config.Config{}, "", fmt.Errorf(
				"usage: /settings tool auto|full|collapsed|hidden",
			)
		}
		updated.ToolVerbosity = verbosity
		notice = "Tool display: " + displayToolVerbosity(verbosity)
	case "read":
		output := config.NormalizeReadOutput(value)
		if output == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings read full|summary|hidden")
		}
		updated.ReadOutput = output
		notice = "Read output: " + displayReadOutput(output)
	case "write":
		output := config.NormalizeWriteOutput(value)
		if output == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings write diff|summary|hidden")
		}
		updated.WriteOutput = output
		notice = "Write output: " + displayWriteOutput(output)
	case "bash":
		output := config.NormalizeBashOutput(value)
		if output == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings bash full|summary|hidden")
		}
		updated.BashOutput = output
		notice = "Bash output: " + displayBashOutput(output)
	case "thinking":
		verbosity := config.NormalizeVerbosity(value)
		if verbosity == "" {
			return config.Config{}, "", fmt.Errorf(
				"usage: /settings thinking full|collapsed|hidden",
			)
		}
		updated.ThinkingVerbosity = verbosity
		notice = "Thinking display: " + verbosity
	case "busy", "busy_input":
		mode := config.NormalizeBusyInput(value)
		if mode == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings busy queue|steer")
		}
		if mode == "queue" {
			updated.BusyInput = ""
		} else {
			updated.BusyInput = mode
		}
		notice = "Busy input: " + mode
	default:
		return config.Config{}, "", fmt.Errorf(
			"usage: /settings [retry|tool|read|write|bash|thinking|busy] ...",
		)
	}
	return updated, notice, nil
}

func (m Model) settingsSummary(cfg *config.Config) string {
	if cfg == nil {
		cfg = &config.Config{}
	}
	return strings.Join([]string{
		"settings",
		"",
		"  retry network errors: " + onOff(cfg.RetryUntilCancelledEnabled()),
		"  tool display: " + displayToolVerbosity(cfg.ToolVerbosity),
		"  read output: " + displayReadOutput(cfg.ReadOutput),
		"  write output: " + displayWriteOutput(cfg.WriteOutput),
		"  bash output: " + displayBashOutput(cfg.BashOutput),
		"  thinking output: " + displayThinkingVerbosity(cfg.ThinkingVerbosity),
		"  busy input: " + cfg.BusyInputMode(),
		"",
		"commands",
		"",
		"  /settings retry on|off",
		"  /settings tool auto|full|collapsed|hidden",
		"  /settings read full|summary|hidden",
		"  /settings write diff|summary|hidden",
		"  /settings bash full|summary|hidden",
		"  /settings thinking full|collapsed|hidden",
		"  /settings busy queue|steer",
	}, "\n")
}

func parseOnOff(value string) (bool, bool) {
	switch value {
	case "on", "true", "yes":
		return true, true
	case "off", "false", "no":
		return false, true
	default:
		return false, false
	}
}

func onOff(enabled bool) string {
	if enabled {
		return "on"
	}
	return "off"
}

func parseToolVerbosity(value string) (string, bool) {
	if strings.EqualFold(strings.TrimSpace(value), "auto") {
		return "", true
	}
	normalized := config.NormalizeVerbosity(value)
	return normalized, normalized != ""
}

func displayToolVerbosity(value string) string {
	if normalized := config.NormalizeVerbosity(value); normalized != "" {
		return normalized
	}
	return "auto"
}

func displayReadOutput(value string) string {
	if normalized := config.NormalizeReadOutput(value); normalized != "" {
		return normalized
	}
	return "summary"
}

func displayWriteOutput(value string) string {
	if normalized := config.NormalizeWriteOutput(value); normalized != "" {
		return normalized
	}
	return "summary"
}

func displayBashOutput(value string) string {
	if normalized := config.NormalizeBashOutput(value); normalized != "" {
		return normalized
	}
	return "hidden"
}

func displayThinkingVerbosity(value string) string {
	if normalized := config.NormalizeVerbosity(value); normalized != "" {
		return normalized
	}
	return "hidden"
}
