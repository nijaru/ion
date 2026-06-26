package app

import (
	"strings"

	"github.com/nijaru/ion/internal/runtime"
)

// Re-export runtime types so app/ code can use Backend, Bootstrap, etc.
type Bootstrap = runtime.Bootstrap
type Backend = runtime.Backend
type Compactor = runtime.Compactor
type ToolSurface = runtime.ToolSurface
type ToolSummarizer = runtime.ToolSummarizer

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
