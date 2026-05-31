package session

import "strings"

type SubmitPreflightInput struct {
	RuntimeRequired bool
	Provider        string
	Model           string
	TotalCost       float64
	MaxSessionCost  float64
}

type SubmitPreflightDecision struct {
	Allowed bool
	Reason  string
}

func DecideSubmitPreflight(input SubmitPreflightInput) SubmitPreflightDecision {
	if input.RuntimeRequired {
		if strings.TrimSpace(input.Provider) == "" {
			return SubmitPreflightDecision{Reason: NoProviderConfiguredStatus()}
		}
		if strings.TrimSpace(input.Model) == "" {
			return SubmitPreflightDecision{Reason: NoModelConfiguredStatus()}
		}
	}
	if reason := BudgetStopReason(BudgetStopInput{
		TotalCost:      input.TotalCost,
		MaxSessionCost: input.MaxSessionCost,
	}); reason != "" {
		return SubmitPreflightDecision{Reason: reason}
	}
	return SubmitPreflightDecision{Allowed: true}
}

func NoProviderConfiguredStatus() string {
	return "No provider configured. Use /provider. Set ION_PROVIDER for scripts."
}

func NoModelConfiguredStatus() string {
	return "No model configured. Use /model. Set ION_MODEL for scripts."
}
