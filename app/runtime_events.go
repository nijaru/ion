package app

import (
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/ion/session"
)

// --- Runtime request management ---

type RuntimeRequestBeginInput struct {
	Current uint64
	Status  string
}

type RuntimeRequestDecision struct {
	RequestID        uint64
	SetLocalStatus   bool
	Status           string
	Matched          bool
	Active           uint64
	ClearLocalStatus bool
}

func BeginRuntimeRequest(input RuntimeRequestBeginInput) RuntimeRequestDecision {
	return RuntimeRequestDecision{
		RequestID:      input.Current + 1,
		SetLocalStatus: true,
		Status:         input.Status,
	}
}

func RuntimeRequestMatches(current, requestID uint64) bool {
	return current == requestID
}

type FinishRuntimeRequestDecision struct {
	Matched          bool
	Active           uint64
	ClearLocalStatus bool
}

func FinishRuntimeRequest(current, requestID uint64) FinishRuntimeRequestDecision {
	if current != requestID {
		return FinishRuntimeRequestDecision{Matched: false, Active: current}
	}
	return FinishRuntimeRequestDecision{
		Matched:          true,
		ClearLocalStatus: true,
	}
}

type ClearRuntimeRequestDecision struct {
	Active           uint64
	ClearLocalStatus bool
}

func ClearRuntimeRequest() ClearRuntimeRequestDecision {
	return ClearRuntimeRequestDecision{}
}

// --- Preflight / budget ---

type SubmitPreflightDecision struct {
	Allowed      bool
	ShouldSubmit bool
	Reason       string
}

type SubmitPreflightInput struct {
	RuntimeRequired bool
	Provider        string
	Model           string
	TotalCost       float64
	MaxSessionCost  float64
	MaxTurnCost     float64
}

func DecideSubmitPreflight(input SubmitPreflightInput) SubmitPreflightDecision {
	if input.MaxSessionCost > 0 && input.TotalCost >= input.MaxSessionCost {
		return SubmitPreflightDecision{
			Reason: formatCostLimit("session", input.TotalCost, input.MaxSessionCost),
		}
	}
	return SubmitPreflightDecision{Allowed: true, ShouldSubmit: true}
}

func BudgetStopReason(input BudgetStopInput) string {
	if input.MaxTurnCost > 0 && input.CurrentTurnCost >= input.MaxTurnCost {
		return formatCostLimit("turn", input.CurrentTurnCost, input.MaxTurnCost)
	}
	if input.MaxSessionCost > 0 && input.TotalCost >= input.MaxSessionCost {
		return formatCostLimit("session", input.TotalCost, input.MaxSessionCost)
	}
	return ""
}

func formatCostLimit(scope string, current, maximum float64) string {
	return fmt.Sprintf("%s cost limit reached ($%.4f/$%.4f)", scope, current, maximum)
}

type BudgetStopInput struct {
	CurrentTurnCost float64
	TotalCost       float64
	MaxTurnCost     float64
	MaxSessionCost  float64
}

// --- Busy-input routing ---

func RouteBusyInput(input BusyInputRouting) string {
	if input.Compacting || !input.Thinking {
		return ""
	}
	mode := input.Mode
	if mode == "" {
		mode = BusyInputRouteSteer
	}
	switch mode {
	case BusyInputRouteSteer:
		if input.SupportsSteering {
			return BusyInputRouteSteer
		}
	case BusyInputRouteFollowUp:
		if input.SupportsFollowUp {
			return BusyInputRouteFollowUp
		}
	}
	return ""
}

type BusyInputRouting struct {
	Mode             string
	Thinking         bool
	Compacting       bool
	SupportsSteering bool
	SupportsFollowUp bool
	Route            string
}

var (
	BusyInputRouteSteer    = "steer"
	BusyInputRouteFollowUp = "follow_up"
)

// --- Event drain ---

type EventDrainInput struct {
	Active         bool
	DrainStartedAt time.Time
	Event          session.Event
}

type EventDrainDecision struct {
	Drain       bool
	Action      string
	FinishDrain bool
}

var EventDrainAwait = "await"

func DecideEventDrain(input EventDrainInput) EventDrainDecision {
	return EventDrainDecision{Drain: input.Active}
}

// --- Error settlement ---

type ErrorSettlementInput struct {
	AwaitTerminal bool
	Err           error
	Thinking      bool
	Compacting    bool
}

type ErrorSettlementDecision struct {
	AwaitNext    bool
	DisplayError string
	EntryContent string
}

func DecideErrorSettlement(input ErrorSettlementInput) ErrorSettlementDecision {
	return ErrorSettlementDecision{DisplayError: input.Err.Error()}
}

// --- Session tree ---

type SessionTree struct {
	Current  session.Entry
	Lineage  []session.Entry
	Children []session.Entry
}

// --- Helpers ---

func IsConversationSessionInfo(e *session.SessionInfoEntry) bool {
	if e == nil {
		return false
	}
	preview := strings.TrimSpace(e.LastPreview)
	if preview == "" {
		return false
	}
	// Skip sessions whose preview is only a slash command.
	if strings.HasPrefix(preview, "/") {
		return false
	}
	// Skip sessions whose name is a slash command.
	if strings.HasPrefix(strings.TrimSpace(e.Name), "/") {
		return false
	}
	return true
}

const (
	NoProviderConfiguredStatus = "No provider configured"
	NoModelConfiguredStatus    = "No model configured"
)
