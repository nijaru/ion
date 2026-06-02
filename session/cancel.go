package session

import "time"

type CancelStartInput struct {
	Reason string
	Now    time.Time
}

type CancelStartDecision struct {
	EntryContent   string
	DrainStartedAt time.Time
	ClearQueued    bool
	Thinking       bool
	Canceling      bool
}

func DecideCancelStart(input CancelStartInput) CancelStartDecision {
	return CancelStartDecision{
		EntryContent:   input.Reason,
		DrainStartedAt: input.Now,
		ClearQueued:    true,
		Thinking:       true,
		Canceling:      true,
	}
}

type CancelFinishInput struct {
	Canceling bool
}

type CancelFinishDecision struct {
	PreserveQueued bool
}

func DecideCancelFinish(input CancelFinishInput) CancelFinishDecision {
	return CancelFinishDecision{PreserveQueued: input.Canceling}
}
