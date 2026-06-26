package app

import (
	"strings"

	"github.com/nijaru/ion/internal/agent"
)

// Re-export agent types so app/ code can use Backend, Bootstrap, etc.
type Bootstrap = agent.Bootstrap
type Backend = agent.Backend
type Compactor = agent.Compactor
type ToolSurface = agent.ToolSurface
type ToolSummarizer = agent.ToolSummarizer

func ToolEnvironmentLabel(value string) string {
	switch strings.TrimSpace(value) {
	case "":
		return ""
	case "inherit":
		return "inherited"
	case "inherit_without_provider_keys":
		return "inherited without provider keys"
	default:
		return strings.TrimSpace(value)
	}
}

func ToolEnvironmentSummary(value string) string {
	label := ToolEnvironmentLabel(value)
	if label == "" {
		return ""
	}
	return "Bash env " + label
}
