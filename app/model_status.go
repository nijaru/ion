package app

import (
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/ion/session"
)

func (m Model) configurationStatus() string {
	decision := m.submitPreflightWithoutBudget()
	if decision.Allowed {
		return ""
	}
	return decision.Reason
}

func (m Model) submitPreflightWithoutBudget() session.SubmitPreflightDecision {
	return session.DecideSubmitPreflight(session.SubmitPreflightInput{
		RuntimeRequired: m.Model.Backend != nil,
		Provider:        m.runtimeProvider(),
		Model:           m.runtimeModel(),
	})
}

func (m Model) submitPreflight() session.SubmitPreflightDecision {
	var maxSessionCost float64
	if m.Model.Config != nil {
		maxSessionCost = m.Model.Config.MaxSessionCost
	}
	return session.DecideSubmitPreflight(session.SubmitPreflightInput{
		RuntimeRequired: m.Model.Backend != nil,
		Provider:        m.runtimeProvider(),
		Model:           m.runtimeModel(),
		TotalCost:       m.Progress.TotalCost,
		MaxSessionCost:  maxSessionCost,
	})
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
	return session.BudgetStopReason(session.BudgetStopInput{
		CurrentTurnCost: m.Progress.CurrentTurnCost,
		TotalCost:       m.Progress.TotalCost,
		MaxTurnCost:     m.Model.Config.MaxTurnCost,
		MaxSessionCost:  m.Model.Config.MaxSessionCost,
	})
}

func (m Model) routingDecision(decision, reason, stopReason string) session.StoreRoutingDecision {
	provider := m.runtimeProvider()
	model := m.runtimeModel()
	var maxSessionCost, maxTurnCost float64
	if m.Model.Config != nil {
		maxSessionCost = m.Model.Config.MaxSessionCost
		maxTurnCost = m.Model.Config.MaxTurnCost
	}
	return session.StoreRoutingDecision{
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

func (m Model) runtimeHeaderLine(_ Backend) string {
	version := strings.TrimSpace(m.App.Version)
	if version == "" {
		version = "v0.0.0"
	}
	return "ion " + version
}
