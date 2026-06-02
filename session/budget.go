package session

import "fmt"

type BudgetStopInput struct {
	CurrentTurnCost float64
	TotalCost       float64
	MaxTurnCost     float64
	MaxSessionCost  float64
}

func BudgetStopReason(input BudgetStopInput) string {
	if input.MaxTurnCost > 0 && input.CurrentTurnCost >= input.MaxTurnCost {
		return fmt.Sprintf(
			"turn cost limit reached ($%.6f / $%.6f)",
			input.CurrentTurnCost,
			input.MaxTurnCost,
		)
	}
	if input.MaxSessionCost > 0 && input.TotalCost >= input.MaxSessionCost {
		return fmt.Sprintf(
			"session cost limit reached ($%.6f / $%.6f)",
			input.TotalCost,
			input.MaxSessionCost,
		)
	}
	return ""
}

type BudgetStopSettlementAction string

const (
	BudgetStopIgnore BudgetStopSettlementAction = "ignore"
	BudgetStopRecord BudgetStopSettlementAction = "record"
	BudgetStopCancel BudgetStopSettlementAction = "cancel"
)

type BudgetStopSettlementInput struct {
	Reason         string
	ExistingReason string
	Thinking       bool
}

type BudgetStopSettlementDecision struct {
	Action       BudgetStopSettlementAction
	Reason       string
	EntryContent string
}

func DecideBudgetStopSettlement(input BudgetStopSettlementInput) BudgetStopSettlementDecision {
	if input.Reason == "" || input.Reason == input.ExistingReason {
		return BudgetStopSettlementDecision{Action: BudgetStopIgnore}
	}
	if !input.Thinking {
		return BudgetStopSettlementDecision{
			Action: BudgetStopRecord,
			Reason: input.Reason,
		}
	}
	return BudgetStopSettlementDecision{
		Action:       BudgetStopCancel,
		Reason:       input.Reason,
		EntryContent: "Canceled: " + input.Reason,
	}
}
