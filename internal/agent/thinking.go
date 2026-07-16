package agent

import "github.com/nijaru/ion/session"

// providerReasoningEffort keeps the explicit provider-default selection in
// the session domain while leaving the provider request field unset.
func providerReasoningEffort(level session.ThinkingLevel) string {
	if level == session.ThinkingAuto {
		return ""
	}
	return string(level)
}
