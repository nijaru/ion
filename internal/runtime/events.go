// Package runtime provides the integration layer between the agent harness
// and the TUI/CLI. It owns runtime state management, preflight decisions,
// busy-input routing, event drain, error settlement, and session bundle operations.
//
// Pi equivalent: pi-coding-agent/core/agent-session.js
package runtime

import (
	"context"
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
	Allowed    bool
	ShouldSubmit bool
	Reason     string
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
	return SubmitPreflightDecision{Allowed: true}
}

var budgetStopReasonStr = "budget_stop"

func BudgetStopReason(input BudgetStopInput) string { return budgetStopReasonStr }

type BudgetStopInput struct {
	Reason          string
	CurrentTurnCost float64
	TotalCost       float64
	MaxTurnCost     float64
	MaxSessionCost  float64
}

// --- Busy-input routing ---

func RouteBusyInput(input BusyInputRouting) string { return input.Route }

type BusyInputRouting struct {
	Mode             string
	Thinking         bool
	Compacting       bool
	SupportsSteering bool
	SupportsFollowUp bool
	Route            string
}

var BusyInputRouteSteer = "steer"
var BusyInputRouteFollowUp = "follow_up"

type BusyInputDecision struct {
	Recall       bool
	ComposerText string
	ClearBackend bool
	Action       string
	NoticeContent string
	FollowUp     []string
}

var BusyInputResultAccepted = "accepted"

type SteeringResult struct{}
type FollowUpResult struct{}

type FollowUpResultInput struct {
	Text               string
	PriorFollowUpCount int
	CurrentFollowUp    []string
	Result             FollowUpResult
	Err                error
}

func DecideSteeringResult(result SteeringResult, err error) BusyInputDecision {
	if err != nil {
		return BusyInputDecision{}
	}
	return BusyInputDecision{Action: BusyInputResultAccepted, NoticeContent: "Steering applied"}
}

func DecideFollowUpResult(input FollowUpResultInput) BusyInputDecision {
	if input.Err != nil {
		return BusyInputDecision{}
	}
	return BusyInputDecision{Action: BusyInputResultAccepted, NoticeContent: "Follow-up queued", FollowUp: []string{input.Text}}
}

type QueuedInputRecallInput struct {
	Text         string
	CurrentDraft string
	Steering     []string
	FollowUp     []string
	BackendOwned bool
}

func DecideQueuedInputRecall(input QueuedInputRecallInput) BusyInputDecision {
	return BusyInputDecision{Action: BusyInputResultAccepted, NoticeContent: "Input recalled"}
}

type QueuedSnapshot struct {
	Steering []string
	FollowUp []string
}

// --- Event drain ---

type EventDrainInput struct {
	Active        bool
	DrainStartedAt time.Time
	Event         session.Event
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
	RoutingStop   *ErrorRoutingStop
	PersistSystem bool
	AwaitNext     bool
	DisplayError  string
	EntryContent  string
}

func DecideErrorSettlement(input ErrorSettlementInput) ErrorSettlementDecision {
	return ErrorSettlementDecision{DisplayError: input.Err.Error()}
}

type ErrorRoutingStop struct {
	Reason     string
	StopReason string
}

// --- Session events that reference runtime state ---

// StatusChange reports a status change (used by TUI for display).
type StatusChange struct {
	session.EntryBase
	Status string
}

func (StatusChange) IsEvent()     {}
func (s StatusChange) When() time.Time { return s.EntryBase.Timestamp }

// QueuedInputUpdate reports queued input changes.
type QueuedInputUpdate struct {
	session.EntryBase
	Snapshot QueuedSnapshot
}

func (QueuedInputUpdate) IsEvent() {}

// StoreRoutingDecision persists a routing decision entry.
type StoreRoutingDecision struct {
	session.EntryBase
	Type           string
	Decision       string
	Reason         string
	ModelSlot      string
	Provider       string
	Model          string
	Reasoning      string
	MaxSessionCost float64
	MaxTurnCost    float64
	SessionCost    float64
	TurnCost       float64
	StopReason     string
	TS             time.Time
}

func (StoreRoutingDecision) IsEvent() {}
func (StoreRoutingDecision) IsEntry() {}
func (e StoreRoutingDecision) ID() string        { return e.EntryBase.ID }
func (e StoreRoutingDecision) ParentID() string   { return e.EntryBase.ParentID }
func (e StoreRoutingDecision) When() time.Time    { return e.EntryBase.Timestamp }

// --- Store persistence entries ---

type StoreSystem struct {
	session.EntryBase
	Type    string
	Content string
	TS      int64
}

func (StoreSystem) IsEntry()                   {}
func (s StoreSystem) ID() string               { return s.EntryBase.ID }
func (s StoreSystem) ParentID() string         { return s.EntryBase.ParentID }
func (s StoreSystem) When() time.Time          { return s.EntryBase.Timestamp }

type StoreStatus struct {
	Status string
	session.EntryBase
	Type    string
	Content string
	TS      int64
}

func (StoreStatus) IsEntry()                   {}
func (s StoreStatus) ID() string               { return s.EntryBase.ID }
func (s StoreStatus) ParentID() string         { return s.EntryBase.ParentID }
func (s StoreStatus) When() time.Time          { return s.EntryBase.Timestamp }

type StoreTokenUsage struct {
	session.EntryBase
	Type   string
	Input  int
	Output int
	Cost   float64
	TS     int64
}

func (StoreTokenUsage) IsEntry()                   {}
func (s StoreTokenUsage) ID() string               { return s.EntryBase.ID }
func (s StoreTokenUsage) ParentID() string         { return s.EntryBase.ParentID }
func (s StoreTokenUsage) When() time.Time          { return s.EntryBase.Timestamp }

// --- Session bundle ---

type SessionBundle struct {
	RootSessionID string
	Sessions      []SessionBundleRecord
	ExportedAt    time.Time
}

type SessionBundleRecord struct {
	Info   session.Session
	Events []session.Entry
}

type SessionBundleExporter interface {
	ExportSessionBundle(ctx context.Context, leafID string) (SessionBundle, error)
}

type SessionBundleImporter interface {
	ImportSessionBundle(ctx context.Context, bundle SessionBundle) (string, error)
}

// --- Session tree ---

type SessionTree struct {
	Current  session.Entry
	Lineage  []session.Entry
	Children []session.Entry
}

type SessionTreeReader interface {
	SessionTree(ctx context.Context, leafID string) (SessionTree, error)
}

// --- Session fork (future feature) ---

type SessionForkOptions struct{}

type SessionForker interface {
	ForkSession(ctx context.Context, parentID string, opts SessionForkOptions) (SessionHandle, error)
}

type SessionHandle interface {
	ID() string
	Session() session.Session
}

// --- Helpers ---

func IsMaterialized(s session.Session) bool { return true }

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

// EntryUser creates a user MessageEntry from content and timestamp.
func EntryUser(content string, ts time.Time) (*session.MessageEntry, string) {
	id := fmt.Sprintf("%d", ts.UnixNano())
	return &session.MessageEntry{
		EntryBase: session.EntryBase{ID: id, Timestamp: ts},
		Message:   session.NewUserText(content, ts),
	}, id
}

var NoProviderConfiguredStatus = "No provider configured"
var NoModelConfiguredStatus = "No model configured"


// --- Type aliases ---

// StoreEvent is an alias for session.Entry, used by persistence layer.
type StoreEvent = session.Entry

// --- Interfaces (moved from session/) ---

// SteeringSession is the interface for sessions that support steering.
type SteeringSession interface {
	SteerTurn(ctx context.Context, text string) (SteeringResult, error)
}

// QueuedInputSession is the interface for sessions that support queued input.
type QueuedInputSession interface {
	FollowUpTurn(ctx context.Context, text string) (FollowUpResult, error)
	ClearQueuedInput(ctx context.Context) (string, error)
}
