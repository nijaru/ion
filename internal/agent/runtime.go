// Package agent — runtime controller.
//
// The Controller is the sole mutation authority for runtime state. It owns:
//   - Phase (lifecycle state machine)
//   - Command queue (typed, bounded, single-writer dispatch)
//   - Model, thinking, tools, queues, session, store
//   - Event hub (snapshot-plus-cursor subscriptions)
//   - Action boundary (external effect gate)
//
// Invariant: "one mutation authority, no I/O on the accept path." The command
// loop goroutine is the single writer. Public methods enqueue typed commands
// and wait for results. Provider/storage I/O happens inside handlers, never
// during acceptance. A small publish mutex guards snapshot publication only.
//
// The turn engine (RunLoop in loop.go) is a stateless function that receives
// an immutable TurnContext snapshot and emits events through a callback. It
// owns no session, store, or lifecycle state.
package agent

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"
	"sync"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// controllerCommandCapacity bounds the typed command queue. A full queue
// returns ErrQueueFull so callers fail closed instead of blocking.
const controllerCommandCapacity = 128

// runtimeOperationCapacity bounds persistence/compaction requests waiting
// behind the single ordered runtime I/O operation. Callers fail explicitly
// once this boundary is full; requests are never silently dropped.
const runtimeOperationCapacity = 64

// Controller is the sole runtime mutation authority. One goroutine (run) owns
// all state transitions. Public methods enqueue typed Commands and receive
// results through reply channels.
//
// The struct deliberately groups fields by ownership concern:
//   - Core state: session, store, model, thinking, tools
//   - Lifecycle: phase, closed, queues
//   - Event delivery: eventHub
//   - Turn execution: runCancel, runDone, pending writes
//   - Safety: actionBoundary, approvals
//   - Host resources: closeResources
type Controller struct {
	// --- Core state (owned by command goroutine) ---
	session   session.Session
	store     session.Store
	durable   session.DurableStore
	tools     map[string]Tool
	active    []string
	model     llm.Model
	thinking  session.ThinkingLevel
	sysprompt string
	log       *slog.Logger
	metrics   *Metrics

	// --- Provider plumbing ---
	stream          func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	auth            func(model llm.Model) (apiKey string, headers map[string]string)
	contextOverflow func(error) bool
	transport       http.RoundTripper
	timeout         time.Duration

	// --- Host resources ---
	promptTemplates map[string]string
	closeResources  []func() error
	resourcesOnce   sync.Once
	resourcesErr    error

	// --- Queues (Pi PendingMessageQueue x3) ---
	steer    []session.Message
	followUp []session.Message
	nextTurn []session.Message
	// turnInputClosed seals steer/follow-up acceptance after the loop's final
	// empty drain. It prevents a late message from leaking into a future turn.
	turnInputClosed bool

	steeringMode     string
	followUpMode     string
	queueCapacity    int
	maxParallelTools int

	// --- Lifecycle (owned by command goroutine, guarded by mu) ---
	phase        Phase
	closed       bool
	mu           sync.Mutex // guards phase, closed, runCancel, runDone
	commands     chan Command
	commandStop  chan struct{}
	nextTurnWake chan struct{}

	// --- Active turn coordination ---
	runCancel        chan struct{} // closed to abort current run
	runCancelOnce    *sync.Once
	runContext       context.Context
	runContextCancel context.CancelFunc
	runDone          chan struct{} // closed when run finishes
	turnWorkers      sync.WaitGroup
	dispatchWorkers  sync.WaitGroup
	operationWorkers sync.WaitGroup
	completions      chan turnCompletion
	runtimeRequests  chan runtimeRequest
	runtimeResults   chan runtimeCompletion
	runtimeBusy      bool
	runtimeBusyDone  chan struct{}
	runtimeQueue     []runtimeRequest
	runtimeContext   context.Context
	runtimeCancel    context.CancelFunc

	// --- Event delivery ---
	eventHub *eventHub
	// activeAssistant and activeTools are the controller-owned render snapshot
	// for a live turn. They cover the interval between event publication and
	// durable MessageEnd/TurnCommit so a lagged frontend can resynchronize
	// without inventing a second transcript.
	activeAssistant          *session.AssistantMessage
	activeAssistantCommitted bool
	activeTools              map[string]ActiveToolSnapshot
	runtimeFailure           string
	snapshotRevision         uint64
	done                     chan struct{}
	closeDone                chan struct{}
	closeErr                 error

	// --- Hooks (Pi on/emitHook pattern) ---
	hooks      map[string][]hookRegistration
	nextHookID uint64

	// --- Buffered session writes during a run ---
	pending []pendingWrite
	// staged contains writes already appended to the active durable turn but
	// not yet acknowledged by TurnCommit. They are requeued if that turn is
	// aborted or its terminal write is indeterminate.
	staged []pendingWrite

	// --- Active turn identity ---
	activeTurnID       string
	activeTurnLeaf     string
	nextTurnToken      uint64
	activeTurnToken    uint64
	reservedTurnTokens map[uint64]struct{}
	canceledTurnTokens map[uint64]struct{}
	turnCommitted      bool
	turnAborted        bool
	settledNextTurns   int
	settledPending     bool

	// --- Thinking state coordination ---
	thinkingPending     bool
	thinkingRollback    session.ThinkingLevel
	thinkingGeneration  uint64
	thinkingRollbackSet bool

	// --- Compaction ---
	compaction                CompactionSettings
	summaryRetry              llm.StreamRetryPolicy
	contextWindow             int
	compactionCancel          context.CancelFunc
	compactionCancelToken     uint64
	nextCompactionCancelToken uint64

	// --- Safety ---
	approvals         *ApprovalBroker
	actionBoundary    ActionBoundary
	actionsEnabled    bool
	requireDurable    bool
	processReconciler tool.ProcessReconciler
}

// Compile-time interface assertions.
var (
	_ Runtime                 = (*Controller)(nil)
	_ TurnCanceler            = (*Controller)(nil)
	_ SessionProjectionReader = (*Controller)(nil)
	_ SessionReader           = (*Controller)(nil)
	_ SessionCatalog          = (*Controller)(nil)
	_ InputHistory            = (*Controller)(nil)
	_ SessionNamer            = (*Controller)(nil)
	_ SessionForker           = (*Controller)(nil)
	_ SessionNavigator        = (*Controller)(nil)
	_ SessionLabels           = (*Controller)(nil)
	_ Compactor               = (*Controller)(nil)
	_ ResourceOwner           = (*Controller)(nil)
	_ ActionRecovery          = (*Controller)(nil)
	_ TurnRecovery            = (*Controller)(nil)
	_ ProcessRecovery         = (*Controller)(nil)
)

// run is the command loop goroutine. It is the sole mutator of Controller
// state. Every Command is dispatched here through the typed handler switch.
// The loop exits when stopCh is closed (by Close).
func (c *Controller) run() {
	defer close(c.done)
	for {
		select {
		case cmd := <-c.commands:
			if !c.beginDispatch() {
				c.rejectCommand(cmd)
				continue
			}
			func() {
				defer c.dispatchWorkers.Done()
				c.dispatch(cmd)
			}()
		case completion := <-c.completions:
			c.handleTurnCompletion(completion)
		case <-c.nextTurnWake:
			c.startNextTurnIfReady()
		case request := <-c.runtimeRequests:
			c.handleRuntimeRequest(request)
		case completion := <-c.runtimeResults:
			c.handleRuntimeCompletion(completion)
		case <-c.commandStop:
			c.rejectQueued()
			c.rejectRuntimeRequests()
			return
		}
	}
}

// dispatch routes a typed Command to its handler. The switch is exhaustive:
// every Command type from commands.go must have a case. If the exhaustive
// linter is enabled, adding a new Command without a case is a compile error.
func (c *Controller) dispatch(cmd Command) {
	switch cmd := cmd.(type) {
	case *PromptCmd:
		c.handlePrompt(cmd)
	case *SteerCmd:
		c.handleSteer(cmd)
	case *FollowUpCmd:
		c.handleFollowUp(cmd)
	case *NextTurnCmd:
		c.handleNextTurn(cmd)
	case *AbortCmd:
		c.handleAbort(cmd)
	case *SetModelCmd:
		c.handleSetModel(cmd)
	case *SetThinkingCmd:
		c.handleSetThinking(cmd)
	case *SetToolsCmd:
		c.handleSetTools(cmd)
	case *ActivateToolsCmd:
		c.handleActivateTools(cmd)
	case *PrepareActionCmd:
		c.handlePrepareAction(cmd)
	case *StartActionCmd:
		c.handleStartAction(cmd)
	case *FinishActionCmd:
		c.handleFinishAction(cmd)
	case *CancelActionCmd:
		c.handleCancelAction(cmd)
	case *UnsettledActionsCmd:
		c.handleUnsettledActions(cmd)
	case *ReconcileActionCmd:
		c.handleReconcileAction(cmd)
	case *RecoverProcessActionsCmd:
		c.handleRecoverProcessActions(cmd)
	case *InterruptedTurnsCmd:
		c.handleInterruptedTurns(cmd)
	case *AbortInterruptedTurnCmd:
		c.handleAbortInterruptedTurn(cmd)
	case *SubscribeCmd:
		c.handleSubscribe(cmd)
	case *CompactCmd:
		c.handleCompact(cmd)
	case *NavigateCmd:
		c.handleNavigate(cmd)
	case *AppendSessionInfoCmd:
		c.handleAppendSessionInfo(cmd)
	case *AppendLabelCmd:
		c.handleAppendLabel(cmd)
	case *GetLabelCmd:
		c.handleGetLabel(cmd)
	case *GetBranchLabelCmd:
		c.handleGetBranchLabel(cmd)
	case *ForkSessionCmd:
		c.handleForkSession(cmd)
	case *ExportSessionBundleCmd:
		c.handleExportSessionBundle(cmd)
	case *ImportSessionBundleCmd:
		c.handleImportSessionBundle(cmd)
	case *SessionProjectionCmd:
		c.handleSessionProjection(cmd)
	case *SessionBranchCmd:
		c.handleSessionBranch(cmd)
	case *SessionBranchAtCmd:
		c.handleSessionBranchAt(cmd)
	case *SessionTreeCmd:
		c.handleSessionTree(cmd)
	case *SessionCatalogListCmd:
		c.handleSessionCatalogList(cmd)
	case *SessionCatalogLookupCmd:
		c.handleSessionCatalogLookup(cmd)
	case *SessionCatalogUpdateCmd:
		c.handleSessionCatalogUpdate(cmd)
	case *InputHistoryGetCmd:
		c.handleInputHistoryGet(cmd)
	case *InputHistoryAddCmd:
		c.handleInputHistoryAdd(cmd)
	default:
		panic(fmt.Sprintf("unhandled command type %T", cmd))
	}
}

// rejectQueued drains the command queue after shutdown, sending errors to
// each pending command's reply channel.
func (c *Controller) rejectQueued() {
	for {
		select {
		case cmd := <-c.commands:
			c.rejectCommand(cmd)
		default:
			return
		}
	}
}

// rejectCommand sends a closed error to a command's reply channel.
func (c *Controller) rejectCommand(cmd Command) {
	switch cmd := cmd.(type) {
	case *PromptCmd:
		c.releaseTurnToken(cmd.TurnToken)
		sendResult(cmd.Reply, PromptResult{Err: turnError(KindInternal, PhaseClosed, RecoveryNone, ErrRuntimeClosed)})
	case *SteerCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *FollowUpCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *NextTurnCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *AbortCmd:
		sendResult(cmd.Reply, AbortResult{Err: turnError(KindInternal, PhaseClosed, RecoveryNone, ErrRuntimeClosed)})
	case *SetModelCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *SetThinkingCmd:
		sendResult(cmd.Reply, ThinkingResult{Err: ErrRuntimeClosed})
	case *SetToolsCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *ActivateToolsCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *SubscribeCmd:
		sendResult(cmd.Reply, SubscribeResult{Err: ErrRuntimeClosed})
	case *CompactCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *NavigateCmd:
		sendResult(cmd.Reply, NavigateCmdResult{Err: ErrRuntimeClosed})
	case *AppendSessionInfoCmd:
		sendResult(cmd.Reply, SessionInfoResult{Err: ErrRuntimeClosed})
	case *AppendLabelCmd:
		sendResult(cmd.Reply, SessionInfoResult{Err: ErrRuntimeClosed})
	case *GetLabelCmd:
		sendResult(cmd.Reply, SessionInfoResult{Err: ErrRuntimeClosed})
	case *GetBranchLabelCmd:
		sendResult(cmd.Reply, SessionInfoResult{Err: ErrRuntimeClosed})
	case *ForkSessionCmd:
		sendResult(cmd.Reply, ForkResult{Err: ErrRuntimeClosed})
	case *ExportSessionBundleCmd:
		sendResult(cmd.Reply, ExportSessionResult{Err: ErrRuntimeClosed})
	case *ImportSessionBundleCmd:
		sendResult(cmd.Reply, ImportSessionResult{Err: ErrRuntimeClosed})
	case *SessionProjectionCmd:
		sendResult(cmd.Reply, SessionProjectionResult{Err: ErrRuntimeClosed})
	case *SessionBranchCmd:
		sendResult(cmd.Reply, SessionBranchResult{Err: ErrRuntimeClosed})
	case *SessionBranchAtCmd:
		sendResult(cmd.Reply, SessionBranchResult{Err: ErrRuntimeClosed})
	case *SessionTreeCmd:
		sendResult(cmd.Reply, SessionTreeResult{Err: ErrRuntimeClosed})
	case *SessionCatalogListCmd:
		sendResult(cmd.Reply, SessionCatalogListResult{Err: ErrRuntimeClosed})
	case *SessionCatalogLookupCmd:
		sendResult(cmd.Reply, SessionCatalogLookupResult{Err: ErrRuntimeClosed})
	case *SessionCatalogUpdateCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *InputHistoryGetCmd:
		sendResult(cmd.Reply, InputHistoryGetResult{Err: ErrRuntimeClosed})
	case *InputHistoryAddCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *PrepareActionCmd:
		sendResult(cmd.Reply, ActionPrepareResult{Err: ErrRuntimeClosed})
	case *StartActionCmd:
		sendResult(cmd.Reply, ActionStartResult{Err: ErrRuntimeClosed})
	case *FinishActionCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *CancelActionCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *UnsettledActionsCmd:
		sendResult(cmd.Reply, UnsettledActionsResult{Err: ErrRuntimeClosed})
	case *ReconcileActionCmd:
		sendResult(cmd.Reply, ReconcileActionResult{Err: ErrRuntimeClosed})
	case *RecoverProcessActionsCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *InterruptedTurnsCmd:
		sendResult(cmd.Reply, InterruptedTurnsResult{Err: ErrRuntimeClosed})
	case *AbortInterruptedTurnCmd:
		sendResult(cmd.Reply, AbortInterruptedTurnResult{Err: ErrRuntimeClosed})
	}
}

// enqueue sends a typed Command to the controller goroutine. It validates
// context cancellation and closed state without performing I/O on the accept
// path. Returns ErrQueueFull if the bounded queue is saturated.
func (c *Controller) enqueue(ctx context.Context, cmd Command) error {
	ctx = commandContext(ctx)
	if ctx != nil {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
	}
	// Keep the closed check and non-blocking send under the same lock. Otherwise
	// Close can drain the command queue and stop the loop between the check and
	// the send, stranding an accepted caller forever.
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return ErrRuntimeClosed
	}
	select {
	case c.commands <- cmd:
		return nil
	default:
		return ErrQueueFull
	}
}

// enqueueSync sends a Command and waits for its typed error result.
func (c *Controller) enqueueSync(ctx context.Context, cmd Command, reply chan error) error {
	ctx = commandContext(ctx)
	if err := c.enqueue(ctx, cmd); err != nil {
		return err
	}
	select {
	case err := <-reply:
		return err
	case <-ctx.Done():
		return ctx.Err()
	}
}

func commandContext(ctx context.Context) context.Context {
	if ctx == nil {
		return context.Background()
	}
	return ctx
}

// runtimeBoundContext combines a caller context with the controller lifetime.
// Host-owned persistence must observe Close even when its public API has no
// caller context or the caller passed context.Background().
func (c *Controller) runtimeBoundContext(parent context.Context) (context.Context, func()) {
	parent = commandContext(parent)
	c.mu.Lock()
	runtimeContext := c.runtimeContext
	c.mu.Unlock()
	if runtimeContext == nil {
		return parent, func() {}
	}
	ctx, cancel := context.WithCancel(parent)
	stop := context.AfterFunc(runtimeContext, cancel)
	return ctx, func() {
		stop()
		cancel()
	}
}

// runtimeRunBoundContext combines a caller context, runtime lifetime, and an
// operation cancel signal. The joined worker observes Close and Abort even
// when the caller passed context.Background().
func (c *Controller) runtimeRunBoundContext(
	parent context.Context,
	runCancel <-chan struct{},
) (context.Context, func()) {
	ctx, releaseRuntime := c.runtimeBoundContext(parent)
	ctx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	go func() {
		defer close(done)
		select {
		case <-runCancel:
			cancel()
		case <-ctx.Done():
		}
	}()
	return ctx, func() {
		cancel()
		releaseRuntime()
		<-done
	}
}

func waitCommandReply[T any](ctx context.Context, reply <-chan T) (T, error) {
	ctx = commandContext(ctx)
	select {
	case result := <-reply:
		return result, nil
	case <-ctx.Done():
		var zero T
		return zero, ctx.Err()
	}
}

// currentPhase returns the current lifecycle phase. Thread-safe for read-only
// projections (e.g., IsIdle).
func (c *Controller) currentPhase() Phase {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.phase
}

// isClosed reports whether the controller has been shut down.
func (c *Controller) isClosed() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.closed
}

// transitionPhase validates and applies a phase transition. Must be called
// only from the command goroutine (the single writer).
func (c *Controller) transitionPhase(from, to Phase) error {
	if !legalTransition(from, to) {
		return fmt.Errorf("%w: %s -> %s", ErrPhaseConflict, from, to)
	}
	c.phase = to
	return nil
}

// beginTurn reserves the controller for a new agent turn. The derived
// context is canceled by Abort and Close as well as by the caller.
// The controller retains the reservation until it processes the worker's
// typed completion. Must be called from the command goroutine.
func (c *Controller) beginTurn(parent context.Context, requestedToken ...uint64) (chan struct{}, error) {
	if parent == nil {
		parent = context.Background()
	}
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil, ErrRuntimeClosed
	}
	token := uint64(0)
	if len(requestedToken) > 0 {
		token = requestedToken[0]
	}
	if token != 0 {
		if _, canceled := c.canceledTurnTokens[token]; canceled {
			delete(c.canceledTurnTokens, token)
			delete(c.reservedTurnTokens, token)
			c.mu.Unlock()
			return nil, context.Canceled
		}
	}
	if c.phase.activeTurn() {
		phase := c.phase
		c.mu.Unlock()
		return nil, fmt.Errorf("%w: phase=%s", ErrTurnActive, phase)
	}
	if err := parent.Err(); err != nil {
		if token != 0 {
			delete(c.reservedTurnTokens, token)
			c.canceledTurnTokens[token] = struct{}{}
			c.steer = nil
			c.followUp = nil
			c.nextTurn = nil
		}
		c.mu.Unlock()
		if token != 0 {
			c.emitQueueUpdate()
		}
		return nil, err
	}
	if c.phase == PhaseSettled {
		if err := c.transitionPhase(PhaseSettled, PhaseReady); err != nil {
			c.mu.Unlock()
			return nil, err
		}
	}
	if err := c.transitionPhase(c.phase, PhaseStarting); err != nil {
		c.mu.Unlock()
		return nil, err
	}
	if err := c.transitionPhase(PhaseStarting, PhaseStreaming); err != nil {
		c.mu.Unlock()
		return nil, err
	}
	if token != 0 {
		delete(c.reservedTurnTokens, token)
	} else {
		c.nextTurnToken++
		token = c.nextTurnToken
	}
	c.activeTurnToken = token
	c.turnInputClosed = false
	c.runtimeFailure = ""
	c.runCancel = make(chan struct{})
	c.runCancelOnce = new(sync.Once)
	turnContext, cancelContext := context.WithCancel(parent)
	c.runContext = turnContext
	c.runContextCancel = cancelContext
	c.runDone = make(chan struct{})
	runDone := c.runDone
	c.turnWorkers.Add(1)
	c.mu.Unlock()
	return runDone, nil
}

// beginExclusive reserves the controller for a non-turn operation (compaction,
// navigation, etc.). Returns a cleanup function that restores idle phase.
func (c *Controller) beginExclusive(phase Phase) (func(), error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil, ErrRuntimeClosed
	}
	if c.phase.activeTurn() || c.phase != PhaseReady {
		current := c.phase
		c.mu.Unlock()
		return nil, fmt.Errorf("controller is busy (phase=%s)", current)
	}
	c.phase = phase
	c.runDone = make(chan struct{})
	c.runCancel = make(chan struct{})
	c.runCancelOnce = new(sync.Once)
	done := c.runDone
	c.mu.Unlock()

	return func() {
		c.mu.Lock()
		if c.runDone == done {
			ready := c.phase == phase &&
				(phase == PhasePersisting || phase == PhaseRecovering)
			if c.phase == phase {
				c.phase = PhaseReady
			}
			c.runCancel = nil
			c.runCancelOnce = nil
			c.activeTurnToken = 0
			close(done)
			c.runDone = nil
			// Exclusive operations share the runtime's busy phase with turn
			// finalization. Publish the return to ready after the operation
			// completes so a subscriber that captured the busy snapshot cannot
			// remain stuck there without a lifecycle event. This is not a turn
			// settlement: no AgentEnd occurred, so use the operation-specific
			// RuntimeReady event instead of Settled.
			if ready && !c.closed {
				c.runtimeFailure = ""
				c.snapshotRevision++
				c.emitLocked(session.RuntimeReady{})
			}
		}
		c.mu.Unlock()
		c.wakeNextTurnStart()
	}, nil
}

func (c *Controller) beginDispatch() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return false
	}
	c.dispatchWorkers.Add(1)
	return true
}

// startOperation runs controller-owned external work while retaining a join
// handle for shutdown. The caller is on the command goroutine, so registering
// the worker under mu prevents Close from racing a late Add with Wait.
func (c *Controller) startOperation(fn func()) {
	c.mu.Lock()
	c.operationWorkers.Add(1)
	c.mu.Unlock()
	go func() {
		defer c.operationWorkers.Done()
		fn()
	}()
}

// startContextOperation runs caller-owned work with both the caller's
// cancellation and the controller lifetime. Every operation selected by the
// command loop must observe Close even when its public API was given
// context.Background; direct functions may add a narrower lifetime when their
// operation has additional cancellation rules.
func (c *Controller) startContextOperation(parent context.Context, fn func(context.Context)) {
	operationCtx, release := c.runtimeBoundContext(parent)
	c.startOperation(func() {
		defer release()
		fn(operationCtx)
	})
}

// startReservedOperation registers a worker while holding the lifecycle lock.
// A phase reservation and its worker must become visible atomically to Close;
// otherwise shutdown can pass Wait before the reserved worker is registered.
func (c *Controller) startReservedOperation(finish func(), fn func()) error {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		finish()
		return ErrRuntimeClosed
	}
	c.operationWorkers.Add(1)
	c.mu.Unlock()
	go func() {
		defer c.operationWorkers.Done()
		fn()
	}()
	return nil
}

// cancelCurrentRun signals the active operation exactly once. Cancellation is
// intentionally separate from lifecycle finalization: the controller remains
// responsible for closing runDone and publishing the terminal result.
func (c *Controller) cancelCurrentRun() {
	c.mu.Lock()
	cancel := c.runCancel
	once := c.runCancelOnce
	cancelContext := c.runContextCancel
	c.mu.Unlock()
	if cancel != nil && once != nil {
		once.Do(func() { close(cancel) })
	}
	if cancelContext != nil {
		cancelContext()
	}
}

// --- Public Runtime interface methods ---

// Subscribe opens an independent bounded event stream.
func (c *Controller) Subscribe(ctx context.Context, after EventCursor) (*EventSubscription, error) {
	ctx = commandContext(ctx)
	reply := make(chan SubscribeResult, 1)
	cmd := &SubscribeCmd{Ctx: ctx, After: after, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	return result.Sub, result.Err
}

// Prompt submits a user message and runs a full agent turn.
func (c *Controller) Prompt(ctx context.Context, text string, images ...session.ImageContent) (session.Message, error) {
	ctx = commandContext(ctx)
	token := c.reserveTurnToken()
	if sink := TurnTokenSinkFromContext(ctx); sink != nil {
		sink(token)
	}
	reply := make(chan PromptResult, 1)
	cmd := &PromptCmd{
		Ctx: ctx, TurnToken: token, Text: text, Images: cloneImageContents(images), Reply: reply,
	}
	if err := c.enqueue(ctx, cmd); err != nil {
		c.releaseTurnToken(token)
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	if result.Err != nil {
		return result.Message, result.Err
	}
	return result.Message, nil
}

// Steer queues a steering message.
func (c *Controller) Steer(text string, images ...session.ImageContent) error {
	reply := make(chan error, 1)
	cmd := &SteerCmd{Text: text, Images: images, Reply: reply}
	if err := c.enqueue(context.Background(), cmd); err != nil {
		return err
	}
	return <-reply
}

// FollowUp queues a follow-up message.
func (c *Controller) FollowUp(text string, images ...session.ImageContent) error {
	reply := make(chan error, 1)
	cmd := &FollowUpCmd{Text: text, Images: images, Reply: reply}
	if err := c.enqueue(context.Background(), cmd); err != nil {
		return err
	}
	return <-reply
}

// NextTurn queues a message for the next turn.
func (c *Controller) NextTurn(text string, images ...session.ImageContent) error {
	reply := make(chan error, 1)
	cmd := &NextTurnCmd{Text: text, Images: images, Reply: reply}
	if err := c.enqueue(context.Background(), cmd); err != nil {
		return err
	}
	return <-reply
}

func (c *Controller) reserveTurnToken() uint64 {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.nextTurnToken++
	token := c.nextTurnToken
	if c.reservedTurnTokens == nil {
		c.reservedTurnTokens = make(map[uint64]struct{})
	}
	if c.canceledTurnTokens == nil {
		c.canceledTurnTokens = make(map[uint64]struct{})
	}
	c.reservedTurnTokens[token] = struct{}{}
	return token
}

func (c *Controller) releaseTurnToken(token uint64) {
	if token == 0 {
		return
	}
	c.mu.Lock()
	delete(c.reservedTurnTokens, token)
	delete(c.canceledTurnTokens, token)
	wake := !c.closed && len(c.reservedTurnTokens) == 0 && len(c.nextTurn) > 0
	c.mu.Unlock()
	if wake {
		c.wakeNextTurnStart()
	}
}

// ActiveTurnToken returns the opaque identity of the currently accepted turn.
// It is zero while no turn is active. The token is read under the same lock
// used by AbortTurn, so a caller can bind an asynchronous cancel command to a
// specific runtime turn without exposing controller state.
func (c *Controller) ActiveTurnToken() uint64 {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.activeTurnToken
}

// Abort cancels the current turn and clears queues. Host shutdown and other
// synchronous runtime owners intentionally retain this unscoped operation.
func (c *Controller) Abort() ([]session.Message, []session.Message, error) {
	return c.abortTurn(0)
}

// AbortTurn cancels and clears queues only when turnToken still identifies the
// active turn. A stale token is rejected atomically with the cancellation
// decision, so a delayed frontend command cannot abort a later turn.
func (c *Controller) AbortTurn(turnToken uint64) ([]session.Message, []session.Message, error) {
	if turnToken == 0 {
		return nil, nil, ErrNoActiveTurn
	}
	return c.abortTurn(turnToken)
}

func (c *Controller) abortTurn(expectedToken uint64) ([]session.Message, []session.Message, error) {
	reply := make(chan AbortResult, 1)
	cmd := &AbortCmd{ExpectedTurnToken: expectedToken, Reply: reply}
	if err := c.enqueue(context.Background(), cmd); err != nil {
		return nil, nil, err
	}
	result := <-reply
	if result.Err != nil {
		return result.Steer, result.FollowUp, result.Err
	}
	return result.Steer, result.FollowUp, nil
}

// SetModel switches the active model.
func (c *Controller) SetModel(model llm.Model) error {
	reply := make(chan error, 1)
	cmd := &SetModelCmd{Model: model, Reply: reply}
	if err := c.enqueue(context.Background(), cmd); err != nil {
		return err
	}
	return <-reply
}

// SetThinking changes the thinking level.
func (c *Controller) SetThinking(ctx context.Context, level session.ThinkingLevel) (string, error) {
	ctx = commandContext(ctx)
	reply := make(chan ThinkingResult, 1)
	cmd := &SetThinkingCmd{Ctx: ctx, Level: level, Reply: reply}
	// Once accepted, setter mutation has one authoritative outcome. Wait for
	// the command result instead of returning early on caller cancellation;
	// the handler checks the context before mutation and persistence is bound to
	// the same runtime lifetime.
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.LeafID, result.Err
}

// SetTools updates the complete tool registry and active set.
func (c *Controller) SetTools(tools []Tool, active []string) error {
	reply := make(chan error, 1)
	cmd := &SetToolsCmd{Tools: tools, Active: active, Reply: reply}
	if err := c.enqueue(context.Background(), cmd); err != nil {
		return err
	}
	return <-reply
}

// ActivateTools adds registered tools to the active set.
func (c *Controller) ActivateTools(ctx context.Context, names []string) error {
	ctx = commandContext(ctx)
	reply := make(chan error, 1)
	cmd := &ActivateToolsCmd{Ctx: ctx, Names: names, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return err
	}
	return <-reply
}

// Close initiates shutdown. This is a direct operation (not through the
// command queue) because it needs to stop the command loop itself. It:
//  1. Sets the closed flag
//  2. Cancels any active run
//  3. Waits for the active turn worker to report completion
//  4. Marks the controller closed and stops command dispatch
//  5. Waits for the command loop and auxiliary workers
//  6. Closes the event hub
func (c *Controller) Close() error {
	c.mu.Lock()
	if c.closed {
		closeDone := c.closeDone
		c.mu.Unlock()
		if closeDone != nil {
			<-closeDone
			c.mu.Lock()
			err := c.closeErr
			c.mu.Unlock()
			return err
		}
		return nil
	}
	c.closed = true
	runtimeCancel := c.runtimeCancel
	c.mu.Unlock()
	// Close the synchronous approval lane independently of turn-context
	// cancellation. A pending decision must be denied even when it was opened
	// with a context that does not belong to the active turn, and new requests
	// must fail closed as soon as the runtime accepts shutdown.
	var approvalErr error
	if c.approvals != nil {
		approvalErr = c.approvals.Close()
	}
	if runtimeCancel != nil {
		runtimeCancel()
	}

	// Cancel any active run.
	c.cancelCurrentRun()

	// Wait for a command already selected by the loop to finish dispatching
	// before joining workers. This closes the gap where a handler could register
	// an operation after operationWorkers.Wait had observed a zero count.
	c.dispatchWorkers.Wait()

	// Keep the command loop alive while the prompt worker publishes its typed
	// completion. The completion handler owns phase, turn identity, and the
	// runDone close; stopping the loop first would strand the worker.
	c.turnWorkers.Wait()

	// Keep the command loop alive while runtime completions acknowledge every
	// operation worker. Stopping it first can strand a worker after it has sent
	// a completion and is waiting for the controller's acknowledgement.
	c.operationWorkers.Wait()

	c.mu.Lock()
	if c.phase != PhaseClosed {
		if err := c.transitionPhase(c.phase, PhaseClosed); err != nil {
			// Shutdown is terminal even for an auxiliary operation whose phase is
			// not yet represented in the canonical transition table.
			c.phase = PhaseClosed
		}
	}
	c.mu.Unlock()

	close(c.commandStop)

	// Wait for the command loop to finish.
	<-c.done

	// Close the event hub.
	if c.eventHub != nil {
		c.eventHub.close()
	}

	c.mu.Lock()
	c.closeErr = approvalErr
	if c.closeDone != nil {
		close(c.closeDone)
	}
	c.mu.Unlock()
	return approvalErr
}
