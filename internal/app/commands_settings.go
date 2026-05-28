package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
)

func (m Model) handleSettingsCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) == 1 {
		return m.openSettingsPicker()
	}
	if len(fields) != 3 {
		return m, cmdError(
			"usage: /settings [retry on|off|tool auto|full|collapsed|hidden|tool_mode coding|read|all|read full|summary|hidden|write diff|summary|hidden|bash full|summary|hidden|thinking full|collapsed|hidden|busy queue|steer]",
		)
	}

	key := strings.ToLower(strings.TrimSpace(fields[1]))
	value := strings.ToLower(strings.TrimSpace(fields[2]))
	if _, _, err := settingsConfigUpdate(&config.Config{}, key, value); err != nil {
		return m, cmdError(err.Error())
	}
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing settings"))
	}
	if m.Model.SettingsRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("changing settings"))
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
	if !msg.hasTransition {
		return m, nil
	}
	var err error
	m, err = m.commitRuntimeTransition(msg.transition)
	if err != nil {
		return m, TransitionErrorCmd(err)
	}
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeSettings {
		items := settingsPickerItems(m.Model.Config)
		m.Picker.Overlay.items = items
		m.Picker.Overlay.cfg = m.Model.Config
		m.pickerReducer().refreshOverlayFilter()
		return m, nil
	}
	return m, m.terminalCommit().Entries(systemEntry(msg.notice))
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
	case "tool_mode", "toolmode", "active_tools":
		mode := config.NormalizeToolMode(value)
		if mode == "coding" && value != "coding" {
			return config.Config{}, "", fmt.Errorf("usage: /settings tool_mode coding|read|all")
		}
		if mode == "coding" {
			updated.ToolMode = ""
		} else {
			updated.ToolMode = mode
		}
		notice = "Tool mode: " + mode
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
	case "reasoning", "reasoning_effort", "thinking_level":
		level := config.NormalizeReasoningEffort(value)
		if level == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings thinking_level auto|off|minimal|low|medium|high|xhigh|max")
		}
		updated.ReasoningEffort = level
		notice = "Thinking level: " + level
	case "busy", "busy_input":
		mode := config.NormalizeBusyInput(value)
		if mode == "" {
			return config.Config{}, "", fmt.Errorf("usage: /settings busy queue|steer")
		}
		if mode == "queue" {
			updated.BusyInput = "queue"
		} else {
			updated.BusyInput = ""
		}
		notice = "Busy input: " + mode
	default:
		return config.Config{}, "", fmt.Errorf(
			"usage: /settings [retry|tool|tool_mode|read|write|bash|thinking|busy|reasoning] ...",
		)
	}
	return updated, notice, nil
}

func (m Model) openSettingsPicker() (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("opening settings"))
	}
	if m.Model.SettingsRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("opening settings"))
	}
	cfg := &config.Config{}
	if m.Model.Config != nil {
		clone := *m.Model.Config
		cfg = &clone
	}
	items := settingsPickerItems(cfg)
	m.clearProgressError()
	m.pickerReducer().openOverlay(pickerOverlayState{
		title:    "Settings",
		items:    items,
		filtered: append([]pickerItem(nil), items...),
		index:    0,
		purpose:  pickerPurposeSettings,
		cfg:      cfg,
	})
	return m, nil
}

func settingsPickerItems(cfg *config.Config) []pickerItem {
	if cfg == nil {
		cfg = &config.Config{}
	}
	retry := onOff(cfg.RetryUntilCancelledEnabled())
	busy := cfg.BusyInputMode()
	toolDisplay := displayToolVerbosity(cfg.ToolVerbosity)
	thinkingOutput := displayThinkingVerbosity(cfg.ThinkingVerbosity)
	reasoning := displayReasoningEffort(cfg.ReasoningEffort)
	toolMode := displayToolMode(cfg.ToolMode)

	return []pickerItem{
		settingsPickerItem(
			"Retry network errors",
			"retry",
			retry,
			toggleOnOff(retry),
			"Turn behavior",
			"Retry transient provider/network failures",
		),
		settingsPickerItem(
			"Active turn input",
			"busy",
			busy,
			toggleBusyInput(busy),
			"Turn behavior",
			"Default running-turn input behavior",
		),
		settingsPickerItem(
			"Thinking level",
			"reasoning",
			reasoning,
			nextSettingValue(reasoning, []string{"auto", "off", "low", "medium", "high"}),
			"Turn behavior",
			"Reasoning depth for thinking models",
		),
		settingsPickerItem(
			"Tool permission",
			"tool_mode",
			toolMode,
			nextSettingValue(toolMode, []string{"coding", "read", "all"}),
			"Turn behavior",
			"Execution rights for agent tools",
		),
		settingsPickerItem(
			"Tool display",
			"tool",
			toolDisplay,
			nextSettingValue(toolDisplay, []string{"collapsed", "full", "hidden"}),
			"Display",
			"Tool call/result visibility",
		),
		settingsPickerItem(
			"Thinking output",
			"thinking",
			thinkingOutput,
			nextSettingValue(thinkingOutput, []string{"hidden", "collapsed", "full"}),
			"Display",
			"Reasoning transcript detail",
		),
	}
}

func settingsPickerItem(label, key, current, next, group, detail string) pickerItem {
	itemLabel := label + ": " + current
	itemDetail := "Enter: " + current + " -> " + next
	if detail != "" {
		itemDetail += " • " + detail
	}
	return pickerItem{
		Label:       itemLabel,
		Value:       key + " " + next,
		Detail:      itemDetail,
		Group:       group,
		SettingName: label,
		CurrentVal:  current,
		Desc:        detail,
		Search: pickerSearchIndex(
			itemLabel,
			key+" "+current+" "+next,
			detail,
			group,
			nil,
		),
	}
}

func toggleOnOff(value string) string {
	if value == "on" {
		return "off"
	}
	return "on"
}

func toggleBusyInput(value string) string {
	if value == "queue" {
		return "steer"
	}
	return "queue"
}

func nextSettingValue(current string, values []string) string {
	if len(values) == 0 {
		return current
	}
	for i, value := range values {
		if value == current {
			return values[(i+1)%len(values)]
		}
	}
	return values[0]
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
	return "collapsed"
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

func displayReasoningEffort(value string) string {
	if normalized := config.NormalizeReasoningEffort(value); normalized != "" {
		return normalized
	}
	return "auto"
}

func displayToolMode(value string) string {
	if normalized := config.NormalizeToolMode(value); normalized != "" {
		return normalized
	}
	return "coding"
}
