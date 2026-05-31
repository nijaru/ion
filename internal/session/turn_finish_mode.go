package session

type TurnFinishModeAction string

const (
	TurnFinishPreserveError TurnFinishModeAction = "preserve_error"
	TurnFinishBudgetCancel  TurnFinishModeAction = "budget_cancel"
	TurnFinishUserCancel    TurnFinishModeAction = "user_cancel"
	TurnFinishMissingAgent  TurnFinishModeAction = "missing_agent"
	TurnFinishComplete      TurnFinishModeAction = "complete"
)

type TurnFinishModeInput struct {
	HadError           bool
	BudgetStopReason   string
	Canceled           bool
	Canceling          bool
	AssistantCompleted bool
}

type TurnFinishModeDecision struct {
	Action       TurnFinishModeAction
	ClearQueued  bool
	DisplayError string
	EntryContent string
}

func DecideTurnFinishMode(input TurnFinishModeInput) TurnFinishModeDecision {
	switch {
	case input.HadError:
		return TurnFinishModeDecision{
			Action:      TurnFinishPreserveError,
			ClearQueued: true,
		}
	case input.BudgetStopReason != "":
		return TurnFinishModeDecision{
			Action:      TurnFinishBudgetCancel,
			ClearQueued: true,
		}
	case input.Canceled:
		cancel := DecideCancelFinish(CancelFinishInput{Canceling: input.Canceling})
		return TurnFinishModeDecision{
			Action:      TurnFinishUserCancel,
			ClearQueued: !cancel.PreserveQueued,
		}
	case !input.AssistantCompleted:
		const displayErr = "turn finished without assistant response"
		return TurnFinishModeDecision{
			Action:       TurnFinishMissingAgent,
			ClearQueued:  true,
			DisplayError: displayErr,
			EntryContent: "Error: " + displayErr,
		}
	default:
		return TurnFinishModeDecision{Action: TurnFinishComplete}
	}
}
