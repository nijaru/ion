package agent

import (
	"errors"
	"fmt"
)

// Phase is the runtime controller lifecycle state. The controller goroutine
// is the sole owner of phase transitions. legalTransition is the authority
// for accepting, rejecting, or queueing commands at each phase.
type Phase uint8

const (
	PhaseReady Phase = iota
	PhaseStarting
	PhaseStreaming
	PhaseAwaitingApproval
	PhaseExecutingTool
	PhasePersisting
	PhaseRecovering
	PhaseSettled
	PhaseClosed
)

func (p Phase) String() string {
	switch p {
	case PhaseReady:
		return "ready"
	case PhaseStarting:
		return "starting"
	case PhaseStreaming:
		return "streaming"
	case PhaseAwaitingApproval:
		return "awaiting-approval"
	case PhaseExecutingTool:
		return "executing-tool"
	case PhasePersisting:
		return "persisting"
	case PhaseRecovering:
		return "recovering"
	case PhaseSettled:
		return "settled"
	case PhaseClosed:
		return "closed"
	}
	return "unknown"
}

// activeTurn reports whether the phase belongs to an in-flight turn.
// Ready and Settled are idle states; Closed is terminal.
func (p Phase) activeTurn() bool {
	switch p {
	case PhaseStarting, PhaseStreaming, PhaseAwaitingApproval,
		PhaseExecutingTool, PhasePersisting, PhaseRecovering:
		return true
	}
	return false
}

// acceptsTurnInput reports phases where user input can still affect the
// running engine. Persisting and recovering are terminal barriers: accepting
// a setter or queue mutation there would publish events after Settled or
// contradict the durable outcome being finalized.
func (p Phase) acceptsTurnInput() bool {
	switch p {
	case PhaseStarting, PhaseStreaming, PhaseAwaitingApproval, PhaseExecutingTool:
		return true
	default:
		return false
	}
}

// legalTransition is the state machine. Pure function, no I/O.
// Every transition must pass this check; the controller enforces it.
func legalTransition(from, to Phase) bool {
	switch from {
	case PhaseReady:
		return to == PhaseStarting || to == PhaseClosed
	case PhaseStarting:
		return to == PhaseStreaming || to == PhaseRecovering || to == PhaseClosed
	case PhaseStreaming:
		return to == PhaseAwaitingApproval || to == PhasePersisting ||
			to == PhaseRecovering || to == PhaseClosed
	case PhaseAwaitingApproval:
		return to == PhaseExecutingTool || to == PhaseRecovering || to == PhaseClosed
	case PhaseExecutingTool:
		return to == PhaseStreaming || to == PhasePersisting ||
			to == PhaseRecovering || to == PhaseClosed
	case PhasePersisting:
		return to == PhaseSettled || to == PhaseRecovering
	case PhaseRecovering:
		return to == PhaseSettled || to == PhaseClosed
	case PhaseSettled:
		return to == PhaseReady || to == PhaseClosed
	case PhaseClosed:
		return false
	}
	return false
}

// mustTransition panics if a transition is illegal. Used only where the
// controller has already validated the transition via legalTransition.
func mustTransition(from, to Phase) Phase {
	if !legalTransition(from, to) {
		panic(fmt.Sprintf("illegal phase transition %s -> %s", from, to))
	}
	return to
}

// ErrorKind classifies the failure source for recovery decisions.
type ErrorKind uint8

const (
	KindProvider ErrorKind = iota
	KindTool
	KindPersistence
	KindCancellation
	KindPolicy
	KindQueue
	KindInternal
)

func (k ErrorKind) String() string {
	switch k {
	case KindProvider:
		return "provider"
	case KindTool:
		return "tool"
	case KindPersistence:
		return "persistence"
	case KindCancellation:
		return "cancellation"
	case KindPolicy:
		return "policy"
	case KindQueue:
		return "queue"
	case KindInternal:
		return "internal"
	}
	return "unknown"
}

// RecoveryAction tells the caller what recovery is available. The controller
// uses this to decide whether to retry, resume, or surface the error.
type RecoveryAction uint8

const (
	RecoveryNone RecoveryAction = iota
	RecoveryRetry
	RecoveryResume
	RecoveryAbort
)

func (r RecoveryAction) String() string {
	switch r {
	case RecoveryNone:
		return "none"
	case RecoveryRetry:
		return "retry"
	case RecoveryResume:
		return "resume"
	case RecoveryAbort:
		return "abort"
	}
	return "unknown"
}

// TurnError is the typed failure returned by controller operations. It
// carries the phase, kind, and recovery action so callers can make
// programmatic decisions instead of string-matching error messages.
type TurnError struct {
	Phase    Phase
	Kind     ErrorKind
	Cause    error
	Recovery RecoveryAction
}

func (e *TurnError) Error() string {
	if e == nil {
		return "unknown error"
	}
	if e.Cause == nil {
		return fmt.Sprintf("%s error at %s phase", e.Kind, e.Phase)
	}
	return fmt.Sprintf("%s error at %s phase: %v", e.Kind, e.Phase, e.Cause)
}

func (e *TurnError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.Cause
}

func turnError(kind ErrorKind, phase Phase, recovery RecoveryAction, cause error) *TurnError {
	return &TurnError{Phase: phase, Kind: kind, Recovery: recovery, Cause: cause}
}

// Typed sentinel errors for common conditions.
var (
	ErrQueueFull      = errors.New("runtime input queue is full")
	ErrRuntimeClosed  = errors.New("runtime is closed")
	ErrPhaseConflict  = errors.New("command conflicts with current phase")
	ErrTurnActive     = errors.New("a turn is already active")
	ErrNoActiveTurn   = errors.New("no active turn")
	ErrActionBoundary = errors.New("external action boundary is unavailable")
)

// Subscription errors — moved here from event_stream.go to consolidate all
// runtime error sentinels in one place.
var (
	ErrSubscriptionLagged = errors.New("runtime event subscription lagged")
	ErrSnapshotChanged    = errors.New("runtime snapshot changed during subscription")
)
