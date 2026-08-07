package agent

import (
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func clampThinkingLevel(model llm.Model, level session.ThinkingLevel) session.ThinkingLevel {
	if level == session.ThinkingAuto || model.Capabilities == nil {
		return level
	}
	if model.Capabilities.SupportsReasoningControl(string(level)) {
		return level
	}
	return session.ThinkingOff
}

// providerReasoningEffort keeps the explicit provider-default selection in
// the session domain while leaving the provider request field unset.
func providerReasoningEffort(level session.ThinkingLevel) string {
	if level == session.ThinkingAuto {
		return ""
	}
	return string(level)
}
