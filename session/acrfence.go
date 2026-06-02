package session

import (
	"fmt"
)

// ReplayAction describes how the tool boundary should handle a replayed call.
type ReplayAction string

const (
	ReplayExecute ReplayAction = "execute"
	ReplayReuse   ReplayAction = "reuse"
)

// ReplayDecision is the ACRFence result for one prospective tool call.
type ReplayDecision struct {
	Action ReplayAction
	Output string
}

// ACRFence validates replay safety for tool execution using durable session
// lifecycle events and idempotency keys.
type ACRFence struct{}

// Validate decides whether the caller should execute the tool, reuse a prior
// completed output, or stop because prior execution is ambiguous.
func (ACRFence) Validate(
	s *Session,
	idempotencyKey string,
) (ReplayDecision, error) {
	record, ok, err := FindToolExecutionByKey(s, idempotencyKey)
	if err != nil {
		return ReplayDecision{}, fmt.Errorf("acrfence: lookup tool execution: %w", err)
	}
	if !ok {
		return ReplayDecision{Action: ReplayExecute}, nil
	}
	if record.Completed.IdempotencyKey != "" {
		return ReplayDecision{
			Action: ReplayReuse,
			Output: record.Completed.Output,
		}, nil
	}
	return ReplayDecision{}, fmt.Errorf(
		"acrfence: prior tool execution for key %q started but did not complete",
		idempotencyKey,
	)
}
