package agent

import (
	"context"

	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Command is the sealed interface for all controller operations. Every public
// runtime operation enters through a typed Command. The controller's handle
// function switches on the concrete type, and the exhaustive linter enforces
// that every command variant has a handler.
//
// Commands carry their own reply channel when a result is needed. The
// controller writes exactly one result before closing the channel. Callers
// that don't need a result use fire-and-forget commands without a reply
// channel.
type Command interface {
	command()
}

// --- Turn commands ---

// PromptCmd submits a user message and runs a full agent turn. The reply
// receives the final assistant message or a typed error.
type PromptCmd struct {
	Ctx       context.Context
	TurnToken uint64
	Text      string
	Images    []session.ImageContent
	Reply     chan<- PromptResult
}

func (PromptCmd) command() {}

type PromptResult struct {
	Message session.Message
	Err     *TurnError
}

// turnCompletion is the only result path from a prompt worker back to the
// controller. The worker performs provider/session I/O, then waits for the
// controller to finalize lifecycle state and acknowledge the completion.
type turnCompletion struct {
	runDone chan struct{}
	reply   chan<- PromptResult
	message session.Message
	runErr  error
	ack     chan struct{}
}

// SteerCmd queues a steering message injected before the next assistant
// response. Returns an error if the runtime is idle or closed.
type SteerCmd struct {
	Text   string
	Images []session.ImageContent
	Reply  chan<- error
}

func (SteerCmd) command() {}

// FollowUpCmd queues a follow-up message for after the agent would stop.
type FollowUpCmd struct {
	Text   string
	Images []session.ImageContent
	Reply  chan<- error
}

func (FollowUpCmd) command() {}

// NextTurnCmd queues a message to start a new turn after settlement.
type NextTurnCmd struct {
	Text   string
	Images []session.ImageContent
	Reply  chan<- error
}

func (NextTurnCmd) command() {}

// AbortCmd cancels the current turn and clears queues. Returns the cleared
// steer and follow-up messages.
type AbortCmd struct {
	// ExpectedTurnToken is zero for the unscoped host/runtime operation. A
	// non-zero token makes cancellation conditional on the observed turn still
	// being active when the command reaches the controller.
	ExpectedTurnToken uint64
	Reply             chan<- AbortResult
}

func (AbortCmd) command() {}

type AbortResult struct {
	Steer    []session.Message
	FollowUp []session.Message
	Err      *TurnError
}

// --- Model and tool commands ---

// SetModelCmd switches the active model. Idle changes are durable before the
// live value changes.
type SetModelCmd struct {
	Model llm.Model
	Reply chan<- error
}

func (SetModelCmd) command() {}

// SetThinkingCmd changes the thinking level.
type SetThinkingCmd struct {
	Ctx   context.Context
	Level session.ThinkingLevel
	Reply chan<- ThinkingResult
}

func (SetThinkingCmd) command() {}

type ThinkingResult struct {
	LeafID string
	Err    error
}

// SetToolsCmd updates the complete tool registry and active set.
type SetToolsCmd struct {
	Tools  []Tool
	Active []string
	Reply  chan<- error
}

func (SetToolsCmd) command() {}

// ActivateToolsCmd adds registered tools to the active set.
type ActivateToolsCmd struct {
	Ctx   context.Context
	Names []string
	Reply chan<- error
}

func (ActivateToolsCmd) command() {}

// PrepareActionCmd routes action planning, durable preparation, approval, and
// authorization through the controller-owned safety coordinator. Tool
// workers never mutate the action journal directly.
type PrepareActionCmd struct {
	Ctx     context.Context
	Request ActionRequest
	Reply   chan<- ActionPrepareResult
}

func (PrepareActionCmd) command() {}

type ActionPrepareResult struct {
	Token *ActionToken
	Err   error
}

// StartActionCmd records the durable start boundary before an executor is
// allowed to cross into an external effect.
type StartActionCmd struct {
	Ctx             context.Context
	Token           ActionToken
	ProcessIdentity string
	Reply           chan<- ActionStartResult
}

func (StartActionCmd) command() {}

type ActionStartResult struct {
	Token *ActionToken
	Err   error
}

// FinishActionCmd records the terminal outcome of a started action.
type FinishActionCmd struct {
	Ctx    context.Context
	Token  ActionToken
	Result ActionResult
	Reply  chan<- error
}

func (FinishActionCmd) command() {}

// CancelActionCmd records cancellation only when the action has not crossed
// its durable start boundary. A started action is conservatively indeterminate.
type CancelActionCmd struct {
	Ctx    context.Context
	Token  ActionToken
	Reason string
	Reply  chan<- error
}

func (CancelActionCmd) command() {}

// UnsettledActionsCmd reads recoverable action evidence through the runtime
// owner rather than exposing the store to frontends.
type UnsettledActionsCmd struct {
	Ctx   context.Context
	Reply chan<- UnsettledActionsResult
}

func (UnsettledActionsCmd) command() {}

type UnsettledActionsResult struct {
	Actions []session.ActionRecord
	Err     error
}

// ReconcileActionCmd records explicit verifier evidence for an indeterminate
// action through the runtime owner.
type ReconcileActionCmd struct {
	Ctx            context.Context
	ActionID       string
	State          session.ActionState
	Verification   string
	ResultIdentity string
	Reason         string
	Cleanup        string
	Reply          chan<- ReconcileActionResult
}

func (ReconcileActionCmd) command() {}

type ReconcileActionResult struct {
	Action session.ActionRecord
	Err    error
}

// RecoverProcessActionsCmd reconciles durable process identities before the
// host presents unsettled actions to a user or allows another turn.
type RecoverProcessActionsCmd struct {
	Ctx   context.Context
	Reply chan<- error
}

func (RecoverProcessActionsCmd) command() {}

// InterruptedTurnsCmd reads durable turns that were recovered after an
// interrupted process. The runtime owns the storage boundary; frontends do
// not query the store directly.
type InterruptedTurnsCmd struct {
	Ctx   context.Context
	Reply chan<- InterruptedTurnsResult
}

func (InterruptedTurnsCmd) command() {}

type InterruptedTurnsResult struct {
	Turns []session.TurnRecord
	Err   error
}

// AbortInterruptedTurnCmd explicitly settles one recovered turn without
// adding its input or staged entries to model-visible conversation history.
type AbortInterruptedTurnCmd struct {
	Ctx    context.Context
	TurnID string
	Reason string
	Reply  chan<- AbortInterruptedTurnResult
}

func (AbortInterruptedTurnCmd) command() {}

type AbortInterruptedTurnResult struct {
	Turn session.TurnRecord
	Err  error
}

// --- Session administration commands ---

// SubscribeCmd opens an independent bounded event stream.
type SubscribeCmd struct {
	Ctx   context.Context
	After EventCursor
	Reply chan<- SubscribeResult
}

func (SubscribeCmd) command() {}

type SubscribeResult struct {
	Sub *EventSubscription
	Err error
}

// CompactCmd requests a context compaction at a safe runtime boundary.
type CompactCmd struct {
	Ctx   context.Context
	Reply chan<- error
}

func (CompactCmd) command() {}

// NavigateCmd moves the active session leaf.
type NavigateCmd struct {
	Ctx    context.Context
	Target string
	Opts   NavigateOptions
	Reply  chan<- NavigateCmdResult
}

func (NavigateCmd) command() {}

type NavigateCmdResult struct {
	Result NavigateResult
	Err    error
}

// AppendSessionInfoCmd updates display metadata for the active session.
type AppendSessionInfoCmd struct {
	Ctx            context.Context
	ExpectedLeafID string
	Name           string
	Reply          chan<- SessionInfoResult
}

func (AppendSessionInfoCmd) command() {}

type SessionInfoResult struct {
	Name string
	Err  error
}

// AppendLabelCmd writes a branch label.
type AppendLabelCmd struct {
	Ctx            context.Context
	ExpectedLeafID string
	Target         string
	Label          string
	Reply          chan<- SessionInfoResult
}

func (AppendLabelCmd) command() {}

// GetLabelCmd reads a branch label.
type GetLabelCmd struct {
	Ctx    context.Context
	Target string
	Reply  chan<- SessionInfoResult
}

func (GetLabelCmd) command() {}

// GetBranchLabelCmd reads the latest label on an explicit branch leaf.
type GetBranchLabelCmd struct {
	Ctx    context.Context
	LeafID string
	Reply  chan<- SessionInfoResult
}

func (GetBranchLabelCmd) command() {}

// ForkSessionCmd creates a new session rooted at a source.
type ForkSessionCmd struct {
	Ctx      context.Context
	SourceID string
	Reply    chan<- ForkResult
}

func (ForkSessionCmd) command() {}

type ForkResult struct {
	ID  string
	Err error
}

// ExportSessionBundleCmd exports a session through the controller-owned
// administration boundary.
type ExportSessionBundleCmd struct {
	Ctx       context.Context
	SessionID string
	Reply     chan<- ExportSessionResult
}

func (ExportSessionBundleCmd) command() {}

type ExportSessionResult struct {
	Bundle ionexport.SessionBundle
	Err    error
}

// ImportSessionBundleCmd imports a session through the controller-owned
// administration boundary.
type ImportSessionBundleCmd struct {
	Ctx    context.Context
	Bundle ionexport.SessionBundle
	Reply  chan<- ImportSessionResult
}

func (ImportSessionBundleCmd) command() {}

type ImportSessionResult struct {
	ID  string
	Err error
}

// SessionProjectionCmd reads the active session through the controller. The
// result is one consistent view for frontend copy, usage, status, and replay
// reads; it never exposes the session or storage owner.
type SessionProjectionCmd struct {
	Ctx   context.Context
	Reply chan<- SessionProjectionResult
}

func (SessionProjectionCmd) command() {}

type SessionProjectionResult struct {
	Projection SessionProjection
	Err        error
}

// SessionBranchCmd reads the current active branch through the controller.
type SessionBranchCmd struct {
	Ctx   context.Context
	Reply chan<- SessionBranchResult
}

func (SessionBranchCmd) command() {}

type SessionBranchResult struct {
	Entries []session.Entry
	Err     error
}

// SessionBranchAtCmd reads a selected branch through the controller. The
// target is a leaf entry in this runtime's session tree; callers receive only
// immutable entries, never the storage owner.
type SessionBranchAtCmd struct {
	Ctx    context.Context
	LeafID string
	Reply  chan<- SessionBranchResult
}

func (SessionBranchAtCmd) command() {}

// SessionTreeCmd reads the active session tree and selected leaf through the
// controller. The app receives a projection, never the store.
type SessionTreeCmd struct {
	Ctx   context.Context
	Reply chan<- SessionTreeResult
}

func (SessionTreeCmd) command() {}

type SessionTreeResult struct {
	Tree SessionTreeSnapshot
	Err  error
}

// SessionSearchCmd executes full-text search across active conversation entries.
type SessionSearchCmd struct {
	Ctx   context.Context
	Query string
	Limit int
	Reply chan<- SessionSearchResult
}

func (SessionSearchCmd) command() {}

type SessionSearchResult struct {
	Results []session.SearchResult
	Err     error
}

// SessionCatalogListCmd lists sessions for a workdir.
type SessionCatalogListCmd struct {
	Ctx     context.Context
	Workdir string
	Reply   chan<- SessionCatalogListResult
}

func (SessionCatalogListCmd) command() {}

type SessionCatalogListResult struct {
	Sessions []session.SessionInfoEntry
	Err      error
}

// SessionCatalogLookupCmd reads one session catalog entry.
type SessionCatalogLookupCmd struct {
	Ctx       context.Context
	SessionID string
	Reply     chan<- SessionCatalogLookupResult
}

func (SessionCatalogLookupCmd) command() {}

type SessionCatalogLookupResult struct {
	Info session.SessionInfoEntry
	Err  error
}

// SessionCatalogUpdateCmd persists one session catalog entry.
type SessionCatalogUpdateCmd struct {
	Ctx   context.Context
	Info  session.SessionInfoEntry
	Reply chan<- error
}

func (SessionCatalogUpdateCmd) command() {}

// SessionCatalogDeleteCmd removes one session catalog entry.
type SessionCatalogDeleteCmd struct {
	Ctx       context.Context
	SessionID string
	Reply     chan<- error
}

func (SessionCatalogDeleteCmd) command() {}

// InputHistoryGetCmd reads bounded composer history.
type InputHistoryGetCmd struct {
	Ctx     context.Context
	Workdir string
	Limit   int
	Reply   chan<- InputHistoryGetResult
}

func (InputHistoryGetCmd) command() {}

type InputHistoryGetResult struct {
	Inputs []string
	Err    error
}

// InputHistoryAddCmd appends one composer history item.
type InputHistoryAddCmd struct {
	Ctx     context.Context
	Workdir string
	Input   string
	Reply   chan<- error
}

func (InputHistoryAddCmd) command() {}

// commandResult is a helper for sending exactly one result on a reply channel.
func sendResult[T any](reply chan<- T, result T) {
	if reply == nil {
		return
	}
	reply <- result
}
