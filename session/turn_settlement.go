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

type TurnFinishedDispatchAction string

const (
	TurnFinishedDispatchAwait       TurnFinishedDispatchAction = "await"
	TurnFinishedDispatchSubmitLocal TurnFinishedDispatchAction = "submit_local"
)

type TurnFinishedDispatchInput struct {
	BackendOwnedQueued bool
	LocalQueuedTurns   []string
}

type TurnFinishedDispatchDecision struct {
	Action             TurnFinishedDispatchAction
	Text               string
	RearmSessionEvents bool
	ReloadGitDiff      bool
	AwaitNext          bool
}

func DecideTurnFinishedDispatch(
	input TurnFinishedDispatchInput,
) TurnFinishedDispatchDecision {
	settlement := DecideTurnSettlement(TurnSettlementInput{
		BackendOwnedQueued: input.BackendOwnedQueued,
		LocalQueuedTurns:   input.LocalQueuedTurns,
	})
	if settlement.Action == TurnSettlementSubmitLocal {
		return TurnFinishedDispatchDecision{
			Action:             TurnFinishedDispatchSubmitLocal,
			Text:               settlement.Text,
			RearmSessionEvents: true,
		}
	}
	return TurnFinishedDispatchDecision{
		Action:        TurnFinishedDispatchAwait,
		ReloadGitDiff: true,
		AwaitNext:     true,
	}
}
