package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

func (m Model) handleSettingsCommand(fields []string) (Model, tea.Cmd) {
	cfg, err := config.LoadStable()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	if len(fields) == 1 {
		return m, m.printEntries(session.Entry{
			Role:    session.System,
			Content: m.settingsSummary(cfg),
		})
	}
	if len(fields) != 3 {
		return m, cmdError(
			"usage: /settings [retry on|off|tool auto|full|collapsed|hidden|read full|summary|hidden|write diff|summary|hidden|bash full|summary|hidden|thinking full|collapsed|hidden]",
		)
	}

	updated := *cfg
	key := strings.ToLower(strings.TrimSpace(fields[1]))
	value := strings.ToLower(strings.TrimSpace(fields[2]))
	var notice string

	switch key {
	case "retry":
		enabled, ok := parseOnOff(value)
		if !ok {
			return m, cmdError("usage: /settings retry on|off")
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
			return m, cmdError("usage: /settings tool auto|full|collapsed|hidden")
		}
		updated.ToolVerbosity = verbosity
		notice = "Tool display: " + displayToolVerbosity(verbosity)
	case "read":
		output := config.NormalizeReadOutput(value)
		if output == "" {
			return m, cmdError("usage: /settings read full|summary|hidden")
		}
		updated.ReadOutput = output
		notice = "Read output: " + displayReadOutput(output)
	case "write":
		output := config.NormalizeWriteOutput(value)
		if output == "" {
			return m, cmdError("usage: /settings write diff|summary|hidden")
		}
		updated.WriteOutput = output
		notice = "Write output: " + displayWriteOutput(output)
	case "bash":
		output := config.NormalizeBashOutput(value)
		if output == "" {
			return m, cmdError("usage: /settings bash full|summary|hidden")
		}
		updated.BashOutput = output
		notice = "Bash output: " + displayBashOutput(output)
	case "thinking":
		verbosity := config.NormalizeVerbosity(value)
		if verbosity == "" {
			return m, cmdError("usage: /settings thinking full|collapsed|hidden")
		}
		updated.ThinkingVerbosity = verbosity
		notice = "Thinking display: " + verbosity
	default:
		return m, cmdError("usage: /settings [retry|tool|read|write|bash|thinking] ...")
	}

	if err := config.Save(&updated); err != nil {
		return m, cmdError(fmt.Sprintf("failed to save config: %v", err))
	}
	runtimeCfg, err := config.Load()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to reload runtime config: %v", err))
	}
	mergeRuntimeSelection(runtimeCfg, m.Model.Config)
	m.Model.Config = runtimeCfg
	m.Model.Backend.SetConfig(runtimeCfg)
	return m, m.printEntries(session.Entry{Role: session.System, Content: notice})
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
		"",
		"commands",
		"",
		"  /settings retry on|off",
		"  /settings tool auto|full|collapsed|hidden",
		"  /settings read full|summary|hidden",
		"  /settings write diff|summary|hidden",
		"  /settings bash full|summary|hidden",
		"  /settings thinking full|collapsed|hidden",
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
