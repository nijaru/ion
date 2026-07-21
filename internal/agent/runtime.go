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
)

// controllerCommandCapacity bounds the typed command queue. A full queue
// returns ErrQueueFull so callers fail closed instead of blocking.
const controllerCommandCapacity = 128

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
	stream    func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	auth      func(model llm.Model) (apiKey string, headers map[string]string)
	transport http.RoundTripper
	timeout   time.Duration

	// --- Host resources ---
	promptTemplates map[string]string
	closeResources  []func() error
	resourcesOnce   sync.Once
	resourcesErr    error

	// --- Queues (Pi PendingMessageQueue x3) ---
	steer    []session.Message
	followUp []session.Message
	nextTurn []session.Message

	steeringMode     string
	followUpMode     string
	queueCapacity    int
	maxParallelTools int

	// --- Lifecycle (owned by command goroutine, guarded by mu) ---
	phase       Phase
	closed      bool
	mu          sync.Mutex // guards phase, closed, runCancel, runDone
	commands    chan Command
	commandStop chan struct{}

	// --- Active turn coordination ---
	runCancel     chan struct{} // closed to abort current run
	runCancelOnce *sync.Once
	runDone       chan struct{} // closed when run finishes
	turnWorkers   sync.WaitGroup
	completions   chan turnCompletion

	// --- Event delivery ---
	eventHub *eventHub
	done     chan struct{}

	// --- Hooks (Pi on/emitHook pattern) ---
	hooks map[string][]HookHandler

	// --- Buffered session writes during a run ---
	pending []pendingWrite

	// --- Active turn identity ---
	activeTurnID   string
	activeTurnLeaf string
	turnCommitted  bool
	turnAborted    bool

	// --- Thinking state coordination ---
	thinkingPending     bool
	thinkingRollback    session.ThinkingLevel
	thinkingGeneration  uint64
	thinkingRollbackSet bool

	// --- Compaction ---
	compaction    CompactionSettings
	contextWindow int

	// --- Safety ---
	approvals      *ApprovalBroker
	actionBoundary ActionBoundary
	actionsEnabled bool
}

// Compile-time interface assertions.
var (
	_ Runtime          = (*Controller)(nil)
	_ SessionOwner     = (*Controller)(nil)
	_ EntryPersister   = (*Controller)(nil)
	_ SessionNamer     = (*Controller)(nil)
	_ SessionForker    = (*Controller)(nil)
	_ SessionNavigator = (*Controller)(nil)
	_ SessionLabels    = (*Controller)(nil)
	_ Compactor        = (*Controller)(nil)
	_ ResourceOwner    = (*Controller)(nil)
	_ ActionRecovery   = (*Controller)(nil)
)

// run is the command loop goroutine. It is the sole mutator of Controller
// state. Every Command is dispatched here through the typed handler switch.
// The loop exits when stopCh is closed (by Close).
func (c *Controller) run() {
	defer close(c.done)
	for {
		select {
		case cmd := <-c.commands:
			c.dispatch(cmd)
		case completion := <-c.completions:
			c.handleTurnCompletion(completion)
		case <-c.commandStop:
			c.rejectQueued()
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
	case *AppendMessageCmd:
		c.handleAppendMessage(cmd)
	case *PersistEntryCmd:
		c.handlePersistEntry(cmd)
	case *ForkSessionCmd:
		c.handleForkSession(cmd)
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
		sendResult(cmd.Reply, ErrRuntimeClosed)
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
	case *AppendMessageCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *PersistEntryCmd:
		sendResult(cmd.Reply, ErrRuntimeClosed)
	case *ForkSessionCmd:
		sendResult(cmd.Reply, ForkResult{Err: ErrRuntimeClosed})
	}
}

// enqueue sends a typed Command to the controller goroutine. It validates
// context cancellation and closed state without performing I/O on the accept
// path. Returns ErrQueueFull if the bounded queue is saturated.
func (c *Controller) enqueue(ctx context.Context, cmd Command) error {
	if ctx != nil {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
	}
	c.mu.Lock()
	closed := c.closed
	c.mu.Unlock()
	if closed {
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

// beginTurn reserves the controller for a new agent turn. The controller
// retains the reservation until it processes the worker's typed completion.
// Must be called from the command goroutine.
func (c *Controller) beginTurn() (chan struct{}, error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil, ErrRuntimeClosed
	}
	if c.phase.activeTurn() {
		phase := c.phase
		c.mu.Unlock()
		return nil, fmt.Errorf("%w: phase=%s", ErrTurnActive, phase)
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
	c.runCancel = make(chan struct{})
	c.runCancelOnce = new(sync.Once)
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
		if c.phase == phase {
			c.phase = PhaseReady
			c.runCancel = nil
			c.runCancelOnce = nil
			if c.runDone == done {
				close(done)
				c.runDone = nil
			}
		}
		c.mu.Unlock()
	}, nil
}

// cancelCurrentRun signals the active operation exactly once. Cancellation is
// intentionally separate from lifecycle finalization: the controller remains
// responsible for closing runDone and publishing the terminal result.
func (c *Controller) cancelCurrentRun() {
	c.mu.Lock()
	cancel := c.runCancel
	once := c.runCancelOnce
	c.mu.Unlock()
	if cancel != nil && once != nil {
		once.Do(func() { close(cancel) })
	}
}

// --- Public Runtime interface methods ---

// Subscribe opens an independent bounded event stream.
func (c *Controller) Subscribe(ctx context.Context, after EventCursor) (*EventSubscription, error) {
	reply := make(chan SubscribeResult, 1)
	cmd := &SubscribeCmd{Ctx: ctx, After: after, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return nil, err
	}
	result := <-reply
	return result.Sub, result.Err
}

// Prompt submits a user message and runs a full agent turn.
func (c *Controller) Prompt(ctx context.Context, text string, images ...session.ImageContent) (session.Message, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	reply := make(chan PromptResult, 1)
	cmd := &PromptCmd{Ctx: ctx, Text: text, Images: cloneImageContents(images), Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return nil, err
	}
	result := <-reply
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

// Abort cancels the current turn and clears queues.
func (c *Controller) Abort() ([]session.Message, []session.Message, error) {
	reply := make(chan AbortResult, 1)
	cmd := &AbortCmd{Reply: reply}
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
func (c *Controller) SetThinking(ctx context.Context, level session.ThinkingLevel) error {
	reply := make(chan error, 1)
	cmd := &SetThinkingCmd{Ctx: ctx, Level: level, Reply: reply}
	return c.enqueueSync(ctx, cmd, reply)
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
	reply := make(chan error, 1)
	cmd := &ActivateToolsCmd{Ctx: ctx, Names: names, Reply: reply}
	return c.enqueueSync(ctx, cmd, reply)
}

// Close initiates shutdown. This is a direct operation (not through the
// command queue) because it needs to stop the command loop itself. It:
//  1. Sets the closed flag
//  2. Cancels any active run
//  3. Waits for the active turn worker to report completion
//  4. Closes commandStop (causes the run loop to exit)
//  5. Waits for the run loop to finish
//  6. Closes the event hub
func (c *Controller) Close() error {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil
	}
	c.closed = true
	c.mu.Unlock()

	// Cancel any active run.
	c.cancelCurrentRun()

	// Keep the command loop alive while the prompt worker publishes its typed
	// completion. The completion handler owns phase, turn identity, and the
	// runDone close; stopping the loop first would strand the worker.
	c.turnWorkers.Wait()

	c.mu.Lock()
	if c.phase != PhaseClosed {
		if err := c.transitionPhase(c.phase, PhaseClosed); err != nil {
			// Shutdown is terminal even for an auxiliary operation whose legacy
			// phase is not yet represented in the canonical transition table.
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

	return nil
}

// debugRun is a temporary debug helper.
