package session

import "time"

type EventDrainAction string

const (
	EventDrainProcess EventDrainAction = "process"
	EventDrainAwait   EventDrainAction = "await"
)

type EventDrainInput struct {
	Active         bool
	DrainStartedAt time.Time
	Event          AgentEvent
}

type EventDrainDecision struct {
	Action      EventDrainAction
	FinishDrain bool
}

func DecideEventDrain(input EventDrainInput) EventDrainDecision {
	if !input.Active {
		return EventDrainDecision{Action: EventDrainProcess}
	}
	switch ev := input.Event.(type) {
	case UserMessageEvent:
		if eventAtOrBefore(ev.Timestamp, input.DrainStartedAt) {
			return EventDrainDecision{Action: EventDrainAwait}
		}
		return EventDrainDecision{Action: EventDrainProcess, FinishDrain: true}
	case TurnStartedEvent:
		if eventAtOrBefore(ev.Timestamp, input.DrainStartedAt) {
			return EventDrainDecision{Action: EventDrainAwait}
		}
		return EventDrainDecision{Action: EventDrainProcess, FinishDrain: true}
	case TurnFinishedEvent:
		return EventDrainDecision{Action: EventDrainProcess, FinishDrain: true}
	default:
		return EventDrainDecision{Action: EventDrainAwait}
	}
}

func eventAtOrBefore(eventTime, drainStartedAt time.Time) bool {
	if drainStartedAt.IsZero() || eventTime.IsZero() {
		return false
	}
	return !eventTime.After(drainStartedAt)
}
