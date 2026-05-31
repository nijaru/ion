package session

type TurnSettlementAction string

const (
	TurnSettlementAwait       TurnSettlementAction = "await"
	TurnSettlementSubmitLocal TurnSettlementAction = "submit_local"
)

type TurnSettlementInput struct {
	BackendOwnedQueued bool
	LocalQueuedTurns   []string
}

type TurnSettlementDecision struct {
	Action TurnSettlementAction
	Text   string
}

func DecideTurnSettlement(input TurnSettlementInput) TurnSettlementDecision {
	if input.BackendOwnedQueued || len(input.LocalQueuedTurns) == 0 {
		return TurnSettlementDecision{Action: TurnSettlementAwait}
	}
	return TurnSettlementDecision{
		Action: TurnSettlementSubmitLocal,
		Text:   input.LocalQueuedTurns[0],
	}
}
