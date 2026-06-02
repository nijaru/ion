package session

import "strings"

type BusyInputRoute string

const (
	BusyInputRouteSteer      BusyInputRoute = "steer"
	BusyInputRouteFollowUp   BusyInputRoute = "follow_up"
	BusyInputRouteLocalQueue BusyInputRoute = "local_queue"
)

type BusyInputRouting struct {
	Mode             string
	Thinking         bool
	Compacting       bool
	SupportsSteering bool
	SupportsFollowUp bool
}

func RouteBusyInput(input BusyInputRouting) BusyInputRoute {
	if !input.Thinking || input.Compacting {
		return BusyInputRouteLocalQueue
	}
	if input.Mode == "steer" && input.SupportsSteering {
		return BusyInputRouteSteer
	}
	if input.SupportsFollowUp {
		return BusyInputRouteFollowUp
	}
	return BusyInputRouteLocalQueue
}

type BusyInputResultAction string

const (
	BusyInputResultAccepted BusyInputResultAction = "accepted"
	BusyInputResultFallback BusyInputResultAction = "fallback"
)

type SteeringResultDecision struct {
	Action        BusyInputResultAction
	NoticeContent string
}

func DecideSteeringResult(result SteeringResult, err error) SteeringResultDecision {
	if err == nil && result.Outcome == SteeringAccepted {
		return SteeringResultDecision{
			Action:        BusyInputResultAccepted,
			NoticeContent: "Steering current turn",
		}
	}
	return SteeringResultDecision{Action: BusyInputResultFallback}
}

type FollowUpResultInput struct {
	Text               string
	PriorFollowUpCount int
	CurrentFollowUp    []string
	Result             QueuedInputResult
	Err                error
}

type FollowUpResultDecision struct {
	Action        BusyInputResultAction
	FollowUp      []string
	NoticeContent string
}

func DecideFollowUpResult(input FollowUpResultInput) FollowUpResultDecision {
	if input.Err != nil || input.Result.Outcome != QueuedInputAccepted {
		return FollowUpResultDecision{Action: BusyInputResultFallback}
	}
	followUp := append([]string(nil), input.CurrentFollowUp...)
	if len(followUp) <= input.PriorFollowUpCount {
		followUp = append(followUp, input.Text)
	}
	return FollowUpResultDecision{
		Action:        BusyInputResultAccepted,
		FollowUp:      followUp,
		NoticeContent: "Queued follow-up",
	}
}

type QueuedInputRecallInput struct {
	CurrentDraft string
	Steering     []string
	FollowUp     []string
	BackendOwned bool
}

type QueuedInputRecallDecision struct {
	Recall       bool
	ComposerText string
	ClearBackend bool
}

func DecideQueuedInputRecall(input QueuedInputRecallInput) QueuedInputRecallDecision {
	if len(input.Steering) == 0 && len(input.FollowUp) == 0 {
		return QueuedInputRecallDecision{}
	}
	all := make([]string, 0, len(input.Steering)+len(input.FollowUp)+1)
	if draft := strings.TrimSpace(input.CurrentDraft); draft != "" {
		all = append(all, draft)
	}
	all = append(all, input.Steering...)
	all = append(all, input.FollowUp...)
	return QueuedInputRecallDecision{
		Recall:       true,
		ComposerText: strings.Join(all, "\n"),
		ClearBackend: input.BackendOwned,
	}
}
