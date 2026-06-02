package app

import (
	"strings"

	"github.com/nijaru/ion/internal/core"
)

// Re-export core types so app/ code can use Backend, Bootstrap, etc.
type Bootstrap = core.Bootstrap
type Backend = core.Backend
type Compactor = core.Compactor
type ToolSurface = core.ToolSurface
type ToolSummarizer = core.ToolSummarizer

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
