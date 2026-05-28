package app

import (
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/storage"
)

func (m Model) configurationStatus() string {
	if m.Model.Backend == nil {
		return ""
	}
	if m.runtimeProvider() == "" {
		return noProviderConfiguredStatus()
	}
	if m.runtimeModel() == "" {
		return noModelConfiguredStatus()
	}
	return ""
}

func (m Model) runningProgressParts() []string {
	parts := []string{}
	if m.Progress.CurrentTurnInput > 0 {
		parts = append(parts, "↑ "+compactCount(m.Progress.CurrentTurnInput))
	}
	if m.Progress.CurrentTurnOutput > 0 {
		parts = append(parts, "↓ "+compactCount(m.Progress.CurrentTurnOutput))
	}
	if !m.Progress.TurnStartedAt.IsZero() {
		parts = append(
			parts,
			fmt.Sprintf("%ds", int(time.Since(m.Progress.TurnStartedAt).Seconds())),
		)
	}
	if m.Model.Config != nil && m.Model.Config.MaxTurnCost > 0 {
		parts = append(
			parts,
			fmt.Sprintf("$%.4f/$%.4f", m.Progress.CurrentTurnCost, m.Model.Config.MaxTurnCost),
		)
	}
	parts = append(parts, "Esc/Ctrl+C to cancel")
	return parts
}

func (m Model) completedProgressParts() []string {
	parts := []string{}
	if m.Progress.LastTurnSummary.Input > 0 {
		parts = append(parts, "↑ "+compactCount(m.Progress.LastTurnSummary.Input))
	}
	if m.Progress.LastTurnSummary.Output > 0 {
		parts = append(parts, "↓ "+compactCount(m.Progress.LastTurnSummary.Output))
	}
	if m.Progress.LastTurnSummary.Cost > 0 {
		parts = append(parts, fmt.Sprintf("$%.4f", m.Progress.LastTurnSummary.Cost))
	}
	if m.Progress.LastTurnSummary.Elapsed > 0 {
		parts = append(parts, fmt.Sprintf("%ds", int(m.Progress.LastTurnSummary.Elapsed.Seconds())))
	}
	return parts
}

func (m Model) costBudgetLabel(cost float64) string {
	if m.Model.Config == nil || m.Model.Config.MaxSessionCost <= 0 {
		if cost <= 0 {
			return ""
		}
		return fmt.Sprintf("$%.3f", cost)
	}
	return fmt.Sprintf("$%.3f/$%.3f", cost, m.Model.Config.MaxSessionCost)
}

func (m Model) configuredBudgetStopReason() string {
	if m.Model.Config == nil {
		return ""
	}
	if m.Model.Config.MaxTurnCost > 0 && m.Progress.CurrentTurnCost >= m.Model.Config.MaxTurnCost {
		return fmt.Sprintf(
			"turn cost limit reached ($%.6f / $%.6f)",
			m.Progress.CurrentTurnCost,
			m.Model.Config.MaxTurnCost,
		)
	}
	return m.configuredSessionBudgetStopReason()
}

func (m Model) configuredSessionBudgetStopReason() string {
	if m.Model.Config == nil {
		return ""
	}
	if m.Model.Config.MaxSessionCost > 0 && m.Progress.TotalCost >= m.Model.Config.MaxSessionCost {
		return fmt.Sprintf(
			"session cost limit reached ($%.6f / $%.6f)",
			m.Progress.TotalCost,
			m.Model.Config.MaxSessionCost,
		)
	}
	return ""
}

type providerLimitError struct {
	reason string
	label  string
	raw    string
}

func classifyProviderLimitError(err error) (providerLimitError, bool) {
	if err == nil {
		return providerLimitError{}, false
	}
	raw := strings.TrimSpace(err.Error())
	if raw == "" {
		return providerLimitError{}, false
	}
	lower := strings.ToLower(raw)
	for _, marker := range []string{
		"context_length_exceeded",
		"context length",
		"maximum context",
		"max context",
		"token limit",
		"too many tokens",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "context_limit",
				label:  "API context limit",
				raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"insufficient_quota",
		"usage limit",
		"quota",
		"billing",
		"credit",
		"credits",
		"balance",
		"spend limit",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "quota_limit",
				label:  "API quota or usage limit",
				raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"status code: 429",
		" 429 ",
		"too many requests",
		"rate limit",
		"rate_limit",
		"requests per",
		"tokens per",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "rate_limit",
				label:  "API rate limit",
				raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"resource_exhausted",
		"overloaded",
		"capacity",
		"temporarily unavailable",
	} {
		if strings.Contains(lower, marker) {
			return providerLimitError{
				reason: "provider_capacity",
				label:  "Provider capacity limit",
				raw:    raw,
			}, true
		}
	}
	// 422 errors are handled by sessionErrorDisplay with a more helpful message
	// that suggests actionable steps (e.g., try /model to switch models).
	// Do not classify them here to avoid overriding that message with raw error text.
	return providerLimitError{}, false
}

func (e providerLimitError) display() string {
	return e.label + ": " + e.raw
}

func (m Model) routingDecision(decision, reason, stopReason string) storage.RoutingDecision {
	provider := m.runtimeProvider()
	model := m.runtimeModel()
	var maxSessionCost, maxTurnCost float64
	if m.Model.Config != nil {
		maxSessionCost = m.Model.Config.MaxSessionCost
		maxTurnCost = m.Model.Config.MaxTurnCost
	}
	return storage.RoutingDecision{
		Type:           "routing_decision",
		Decision:       decision,
		Reason:         reason,
		ModelSlot:      m.activePreset().String(),
		Provider:       provider,
		Model:          model,
		Reasoning:      normalizeThinkingValue(m.Progress.ReasoningEffort),
		MaxSessionCost: maxSessionCost,
		MaxTurnCost:    maxTurnCost,
		SessionCost:    m.Progress.TotalCost,
		TurnCost:       m.Progress.CurrentTurnCost,
		StopReason:     stopReason,
		TS:             now(),
	}
}

func (m Model) runtimeHeaderLine(_ backend.Backend) string {
	version := strings.TrimSpace(m.App.Version)
	if version == "" {
		version = "v0.0.0"
	}
	return "ion " + version
}
