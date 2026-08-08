package agent

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// runtimeOperation describes the small set of controller-mediated operations
// that cross the runtime's external I/O boundary. The command loop accepts the
// request and applies the completion; the operation worker never mutates
// controller state or publishes an event.
type runtimeOperation uint8

const (
	runtimePublish runtimeOperation = iota
	runtimeBeginTurn
	runtimePersistMessage
	runtimeFlushPending
	runtimeFinalizeTurn
	runtimeAbortTurn
	runtimeSnapshot
	runtimeCompact
)

type runtimeRequest struct {
	kind      runtimeOperation
	ctx       context.Context
	event     session.Event
	message   session.Message
	timestamp time.Time
	input     string
	images    []session.ImageContent
	reason    string
	failed    bool
	force     bool
	reply     chan<- runtimeResult

	// returnOnCallerCancellation lets an explicit shutdown stop waiting for a
	// durable operation without canceling that operation. Its worker remains
	// runtime-bound so accepted pending writes retain their publication
	// contract and can still be joined by a later Close.
	returnOnCallerCancellation bool

	// The command loop captures these fields before starting external work. They
	// are immutable operation inputs, not a second copy of controller state.
	turnID         string
	parentID       string
	runCancel      <-chan struct{}
	sess           session.Session
	store          session.Store
	durable        session.DurableStore
	requireDurable bool
	pending        []pendingWrite
	staged         []pendingWrite
	nextTurns      int
	hadPending     bool
	autoCompact    bool
	compaction     CompactionSettings
	contextWindow  int
	model          llm.Model
	thinking       session.ThinkingLevel
	auth           func(llm.Model) (string, map[string]string)
	stream         func(context.Context, *llm.Request) (llm.Stream, error)
	release        func()
}

type runtimeResult struct {
	snapshot      session.ContextSnapshot
	leafID        string
	succeeded     int
	failedIndex   int
	turnCommitted bool
	turnAborted   bool
	turn          session.TurnRecord
	warning       error
	err           error
}

type runtimeCompletion struct {
	request runtimeRequest
	result  runtimeResult
	ack     chan struct{}
}

// requestRuntime submits work to the controller and waits for its typed
// result. The reply is buffered so a caller whose context is canceled cannot
// strand the controller completion.
func (c *Controller) requestRuntime(ctx context.Context, request runtimeRequest) runtimeResult {
	ctx = commandContext(ctx)
	request.ctx = ctx
	reply := make(chan runtimeResult, 1)
	request.reply = reply

	// Keep the closed check and non-blocking send under the same lock. This
	// prevents Close from draining the queue and stopping the command loop
	// between the check and the send.
	c.mu.Lock()
	if c.closed && request.kind != runtimeAbortTurn {
		c.mu.Unlock()
		return runtimeResult{err: ErrRuntimeClosed}
	}
	select {
	case c.runtimeRequests <- request:
		c.mu.Unlock()
	default:
		c.mu.Unlock()
		if !runtimeMustComplete(request.kind) {
			select {
			case <-ctx.Done():
				return runtimeResult{err: ctx.Err()}
			default:
			}
		}
		return runtimeResult{err: ErrQueueFull}
	}

	if runtimeMustComplete(request.kind) && !request.returnOnCallerCancellation {
		select {
		case result := <-reply:
			return result
		case <-c.done:
			return runtimeResult{err: ErrRuntimeClosed}
		}
	}
	select {
	case result := <-reply:
		return result
	case <-ctx.Done():
		return runtimeResult{err: ctx.Err()}
	case <-c.done:
		return runtimeResult{err: ErrRuntimeClosed}
	}
}

func runtimeMustComplete(kind runtimeOperation) bool {
	switch kind {
	case runtimePublish,
		runtimePersistMessage,
		runtimeFlushPending,
		runtimeFinalizeTurn,
		runtimeAbortTurn,
		runtimeCompact:
		return true
	default:
		return false
	}
}

func (c *Controller) requestEvent(ctx context.Context, event session.Event) error {
	result := c.requestRuntime(ctx, runtimeRequest{kind: runtimePublish, event: event})
	return result.err
}

func (c *Controller) requestContextSnapshot(ctx context.Context) (session.ContextSnapshot, error) {
	result := c.requestRuntime(ctx, runtimeRequest{kind: runtimeSnapshot})
	return result.snapshot, result.err
}

// handleRuntimeRequest runs on the controller goroutine. It captures the
// active turn identity and pending writes before handing external work to a
// joined operation worker.
func (c *Controller) handleRuntimeRequest(request runtimeRequest) {
	if request.kind == runtimePublish {
		if c.isClosed() {
			sendResult(request.reply, runtimeResult{err: ErrRuntimeClosed})
			return
		}
		c.mu.Lock()
		if c.runtimeBusy {
			if len(c.runtimeQueue) >= runtimeOperationCapacity {
				c.mu.Unlock()
				sendResult(request.reply, runtimeResult{err: ErrQueueFull})
				return
			}
			c.runtimeQueue = append(c.runtimeQueue, request)
			c.mu.Unlock()
			return
		}
		c.runtimeBusy = true
		c.mu.Unlock()
		c.handleRuntimePublish(request)
		return
	}
	if request.kind == runtimeFinalizeTurn {
		// Finalization is a lifecycle barrier, not merely another storage job.
		// Move into persisting before the request can queue so commands accepted
		// after this point cannot mutate the turn while its terminal outcome is
		// being decided.
		c.mu.Lock()
		if c.phase == PhaseStreaming || c.phase == PhaseExecutingTool || c.phase == PhaseAwaitingApproval {
			_ = c.transitionPhase(c.phase, PhasePersisting)
		}
		c.mu.Unlock()
	}

	c.mu.Lock()
	if c.runtimeBusy {
		if len(c.runtimeQueue) >= runtimeOperationCapacity {
			c.mu.Unlock()
			sendResult(request.reply, runtimeResult{err: ErrQueueFull})
			return
		}
		c.runtimeQueue = append(c.runtimeQueue, request)
		c.mu.Unlock()
		return
	}
	prepared, err := c.prepareRuntimeRequestLocked(request)
	if err != nil {
		c.mu.Unlock()
		sendResult(request.reply, runtimeResult{err: err})
		return
	}
	c.runtimeBusy = true
	c.mu.Unlock()
	c.startRuntimeOperation(prepared)
}

func (c *Controller) handleRuntimePublish(request runtimeRequest) {
	c.emit(request.event)
	sendResult(request.reply, runtimeResult{})
	c.startNextRuntimeOperation()
}

// prepareRuntimeRequestLocked snapshots controller-owned inputs and removes
// pending writes from the live queue. The caller must hold c.mu.
func (c *Controller) prepareRuntimeRequestLocked(request runtimeRequest) (runtimeRequest, error) {
	if c.closed && request.kind != runtimeAbortTurn {
		return runtimeRequest{}, ErrRuntimeClosed
	}
	request.turnID = c.activeTurnID
	request.parentID = c.activeTurnLeaf
	request.runCancel = c.runCancel
	request.sess = c.session
	request.store = c.store
	request.durable = c.durable
	request.requireDurable = c.requireDurable
	if request.kind == runtimeFlushPending || request.kind == runtimeFinalizeTurn || request.kind == runtimeAbortTurn {
		request.hadPending = len(c.pending) > 0
		request.pending = c.pending
		c.pending = nil
	}
	if request.kind == runtimeFinalizeTurn || request.kind == runtimeAbortTurn {
		request.staged = c.staged
		c.staged = nil
	}
	if request.kind == runtimeFinalizeTurn {
		request.nextTurns = len(c.nextTurn)
	}
	if (request.kind == runtimeFlushPending && request.event != nil) ||
		request.kind == runtimeFinalizeTurn || request.kind == runtimeCompact {
		// Non-durable turns can compact at their turn-end save point. Durable
		// turns must wait until CommitTurn succeeds, so runtimeFinalizeTurn
		// carries the same immutable compaction inputs into that post-commit
		// boundary.
		request.autoCompact = request.kind == runtimeFinalizeTurn || c.activeTurnID == "" || c.durable == nil
		request.compaction = c.compaction
		request.contextWindow = c.contextWindow
		request.model = c.model
		request.thinking = c.thinking
		request.auth = c.auth
		request.stream = c.stream
	}
	if request.kind == runtimeAbortTurn && request.turnID == "" {
		return runtimeRequest{}, ErrNoActiveTurn
	}
	parent := c.runtimeContext
	if parent == nil {
		parent = context.Background()
	}
	operationContext, cancel := context.WithCancel(parent)
	// Once accepted, persistence is a lifecycle operation. Caller/turn
	// cancellation must not make a successful append or commit invisible to
	// the worker that is waiting for its typed result; Close cancels the shared
	// runtime context when the host must force shutdown. Read/setup operations
	// remain caller-cancelable.
	stopCaller := func() bool { return true }
	if request.kind != runtimePersistMessage && request.kind != runtimeFlushPending &&
		request.kind != runtimeFinalizeTurn && request.kind != runtimeAbortTurn {
		callerContext := request.ctx
		if callerContext == nil {
			callerContext = context.Background()
		}
		stopCaller = context.AfterFunc(callerContext, cancel)
	}
	request.ctx = operationContext
	if request.kind == runtimeCompact {
		c.compactionCancel = cancel
	}
	request.release = func() {
		if request.kind == runtimeCompact {
			c.mu.Lock()
			c.compactionCancel = nil
			c.compactionCancelToken = 0
			c.mu.Unlock()
		}
		stopCaller()
		cancel()
	}
	return request, nil
}

func (c *Controller) startRuntimeOperation(request runtimeRequest) {
	c.startOperation(func() {
		result := executeRuntimeRequest(request)
		completion := runtimeCompletion{
			request: request,
			result:  result,
			ack:     make(chan struct{}),
		}
		c.runtimeResults <- completion
		<-completion.ack
	})
}

// handleRuntimeCompletion is the sole place where a persistence result is
// reflected into controller state and where persisted lifecycle events become
// visible to subscribers.
func (c *Controller) handleRuntimeCompletion(completion runtimeCompletion) {
	request := completion.request
	result := completion.result

	if request.kind == runtimeFlushPending || request.kind == runtimeFinalizeTurn || request.kind == runtimeAbortTurn {
		c.applyPendingResult(request, result)
	}

	if result.leafID != "" {
		c.mu.Lock()
		if c.activeTurnID == request.turnID {
			c.activeTurnLeaf = result.leafID
		}
		c.mu.Unlock()
	}
	if request.kind == runtimeBeginTurn && result.err == nil {
		c.mu.Lock()
		c.activeTurnID = result.turn.ID
		c.activeTurnLeaf = result.turn.LeafID
		c.turnCommitted = false
		c.turnAborted = false
		c.mu.Unlock()
	}

	if result.err == nil {
		switch request.kind {
		case runtimePersistMessage:
			c.logMessage(request.message)
			c.emit(request.event)
		case runtimeFlushPending:
			if request.event != nil {
				c.emit(session.SavePoint{HadPendingMutations: request.hadPending})
				if result.warning != nil {
					c.emit(&session.Error{Err: result.warning})
				}
				c.emit(request.event)
			}
		case runtimeFinalizeTurn:
			c.mu.Lock()
			if request.turnID == c.activeTurnID {
				c.turnCommitted = result.turnCommitted
				c.turnAborted = result.turnAborted
			}
			c.mu.Unlock()
			if result.warning != nil {
				c.emit(&session.Error{Err: result.warning})
			}
			c.emit(request.event)
			c.emit(session.Settled{NextTurnCount: request.nextTurns})
		}
	}

	sendResult(request.reply, result)
	if request.release != nil {
		request.release()
	}
	close(completion.ack)
	c.startNextRuntimeOperation()
}

func (c *Controller) startNextRuntimeOperation() {
	for {
		c.mu.Lock()
		if len(c.runtimeQueue) == 0 {
			c.runtimeBusy = false
			c.mu.Unlock()
			return
		}
		next := c.runtimeQueue[0]
		c.runtimeQueue = c.runtimeQueue[1:]
		if next.kind == runtimePublish {
			c.mu.Unlock()
			c.handleRuntimePublish(next)
			return
		}
		prepared, err := c.prepareRuntimeRequestLocked(next)
		c.mu.Unlock()
		if err != nil {
			sendResult(next.reply, runtimeResult{err: err})
			continue
		}
		c.startRuntimeOperation(prepared)
		return
	}
}

func (c *Controller) rejectRuntimeRequests() {
	for {
		select {
		case request := <-c.runtimeRequests:
			sendResult(request.reply, runtimeResult{err: ErrRuntimeClosed})
		default:
			c.mu.Lock()
			queued := c.runtimeQueue
			c.runtimeQueue = nil
			c.runtimeBusy = false
			c.mu.Unlock()
			for _, request := range queued {
				sendResult(request.reply, runtimeResult{err: ErrRuntimeClosed})
			}
			return
		}
	}
}

// applyPendingResult applies callbacks and preserves retry order on failure.
// These callbacks are controller transitions; they intentionally do not run in
// the operation worker alongside storage I/O.
func (c *Controller) applyPendingResult(request runtimeRequest, result runtimeResult) {
	pending := request.pending
	staged := request.staged
	if len(pending) == 0 && len(staged) == 0 {
		return
	}

	// A successful append inside a durable turn is only staged. Its callback
	// cannot run until TurnCommit succeeds because replay intentionally hides
	// every entry in an aborted or indeterminate turn.
	if request.kind == runtimeFlushPending && request.durable != nil && request.turnID != "" {
		succeeded := minInt(result.succeeded, len(pending))
		if succeeded > 0 {
			c.mu.Lock()
			c.staged = append(c.staged, pending[:succeeded]...)
			c.mu.Unlock()
		}
		if result.err != nil {
			failed := result.failedIndex
			if failed < 0 || failed >= len(pending) {
				failed = succeeded
			}
			c.requeuePending(pending[failed:])
		}
		return
	}

	all := make([]pendingWrite, 0, len(staged)+len(pending))
	all = append(all, staged...)
	all = append(all, pending...)

	if request.kind == runtimeFinalizeTurn {
		if result.err == nil && result.turnCommitted {
			c.acknowledgePending(all)
			return
		}
		if request.durable != nil && request.turnID != "" {
			// Preserve the live requested state for a later turn. The entries
			// written to this turn are not replayable after abort, and callbacks
			// must not claim that they became durable.
			c.requeuePending(all)
			return
		}
	}

	if request.kind == runtimeAbortTurn && request.durable != nil && request.turnID != "" {
		// A durable abort discards this turn from replay. Keep the logical
		// writes queued for an explicit later turn instead of silently losing
		// a user-requested model/tool/thinking change.
		c.requeuePending(all)
		return
	}
	if request.kind == runtimeAbortTurn && request.durable == nil && result.err == nil {
		for _, write := range all {
			if write.onFailure != nil {
				write.onFailure()
			}
		}
		return
	}

	if result.err == nil {
		c.acknowledgePending(all)
		return
	}

	// Non-durable stores have no turn rollback boundary. Preserve the previous
	// retry behavior for them: acknowledge the successful prefix, invoke the
	// failed callback, and retain the failed suffix in order.
	succeeded := minInt(result.succeeded, len(pending))
	c.acknowledgePending(pending[:succeeded])
	if result.failedIndex >= 0 && result.failedIndex < len(pending) {
		if pending[result.failedIndex].onFailure != nil {
			pending[result.failedIndex].onFailure()
		}
		c.requeuePending(pending[result.failedIndex:])
	}
}

func (c *Controller) acknowledgePending(pending []pendingWrite) {
	for _, write := range pending {
		if write.onSuccess != nil {
			write.onSuccess()
		}
	}
}

func (c *Controller) requeuePending(pending []pendingWrite) {
	if len(pending) == 0 {
		return
	}
	c.mu.Lock()
	c.pending = append(append([]pendingWrite(nil), pending...), c.pending...)
	c.mu.Unlock()
}

func executeRuntimeRequest(request runtimeRequest) runtimeResult {
	ctx := request.ctx
	if ctx == nil {
		ctx = context.Background()
	}
	switch request.kind {
	case runtimeBeginTurn:
		if request.requireDurable && request.durable == nil {
			return runtimeResult{err: errors.New("durable runtime requires a DurableStore")}
		}
		if request.durable == nil {
			return runtimeResult{}
		}
		leaf := ""
		if request.sess != nil {
			leaf = request.sess.GetLeafID()
		}
		turn, err := request.durable.BeginTurn(ctx, session.NewEntryID(), request.input, request.images, leaf)
		return runtimeResult{turn: turn, leafID: turn.LeafID, err: err}

	case runtimePersistMessage:
		if request.durable != nil {
			timestamp := request.timestamp
			if timestamp.IsZero() {
				timestamp = time.Now()
			}
			entry := &session.MessageEntry{
				EntryBase: session.EntryBase{
					ID:        session.NewEntryID(),
					ParentID:  request.parentID,
					Timestamp: timestamp,
				},
				Message: request.message,
			}
			id, err := request.durable.AppendTurnEntry(ctx, request.turnID, entry)
			return runtimeResult{leafID: id, err: err}
		}
		if request.sess == nil {
			return runtimeResult{err: errors.New("session is unavailable")}
		}
		id, err := request.sess.AppendMessage(ctx, request.message)
		return runtimeResult{leafID: id, err: err}

	case runtimeFlushPending:
		result := executePendingWrites(request)
		if result.err == nil && request.autoCompact && request.sess != nil &&
			ShouldCompactAfterTurn(ctx, request.sess, request.contextWindow, request.compaction) {
			_, result.warning = runCompaction(ctx, request)
			if result.warning != nil {
				result.warning = fmt.Errorf("auto-compact: %w", result.warning)
			}
		}
		return result

	case runtimeFinalizeTurn:
		if request.requireDurable && request.durable == nil {
			return runtimeResult{err: errors.New("durable runtime requires a DurableStore")}
		}
		result := executePendingWrites(request)
		if result.err != nil {
			return result
		}
		if request.durable == nil || request.turnID == "" {
			result.turnCommitted = true
			return result
		}
		if request.failed {
			result.err = request.durable.AbortTurn(ctx, request.turnID, request.reason)
			result.turnAborted = result.err == nil
			return result
		}
		result.err = request.durable.CommitTurn(ctx, request.turnID)
		result.turnCommitted = result.err == nil
		if result.err == nil && result.turnCommitted && request.autoCompact && request.sess != nil &&
			ShouldCompactAfterTurn(ctx, request.sess, request.contextWindow, request.compaction) {
			compactionCtx, release := contextWithSignalAndRelease(ctx, request.runCancel)
			_, result.warning = runCompaction(compactionCtx, request)
			release()
			if result.warning != nil && !errors.Is(result.warning, context.Canceled) {
				result.warning = fmt.Errorf("auto-compact: %w", result.warning)
			}
		}
		return result

	case runtimeAbortTurn:
		if request.durable == nil || request.turnID == "" {
			return runtimeResult{turnAborted: true}
		}
		result := runtimeResult{}
		result.err = request.durable.AbortTurn(ctx, request.turnID, request.reason)
		result.turnAborted = result.err == nil
		return result

	case runtimeSnapshot:
		if request.durable != nil && request.turnID != "" {
			entries, err := request.durable.TurnBranch(ctx, request.turnID)
			if err != nil {
				return runtimeResult{err: err}
			}
			snapshot, err := session.ProjectContext(entries)
			return runtimeResult{snapshot: snapshot, err: err}
		}
		if request.sess == nil {
			return runtimeResult{err: errors.New("session is unavailable")}
		}
		snapshot, err := request.sess.BuildContext(ctx)
		return runtimeResult{snapshot: snapshot, err: err}

	case runtimeCompact:
		if request.sess == nil {
			return runtimeResult{err: errors.New("session is unavailable")}
		}
		if !request.force && !ShouldCompactAfterTurn(ctx, request.sess, request.contextWindow, request.compaction) {
			return runtimeResult{}
		}
		_, err := runCompaction(ctx, request)
		if err != nil {
			return runtimeResult{err: fmt.Errorf("compact: %w", err)}
		}
		return runtimeResult{}
	}
	return runtimeResult{err: fmt.Errorf("unknown runtime operation %d", request.kind)}
}

func runCompaction(ctx context.Context, request runtimeRequest) (*CompactionResult, error) {
	var apiKey string
	var headers map[string]string
	if request.auth != nil {
		apiKey, headers = request.auth(request.model)
	}
	return Compact(ctx, request.sess, CompactOptions{
		Model:          request.model.ID,
		ModelMaxTokens: request.model.MaxTokens,
		APIKey:         apiKey,
		Headers:        headers,
		ThinkingLevel:  request.thinking,
		Convert:        DefaultConvert,
		StreamFn:       request.stream,
		ContextWindow:  request.contextWindow,
	}, request.compaction)
}

func executePendingWrites(request runtimeRequest) runtimeResult {
	result := runtimeResult{failedIndex: -1}
	leaf := request.parentID
	for i, write := range request.pending {
		var (
			id  string
			err error
		)
		if request.durable != nil && request.turnID != "" {
			if write.applyTurn == nil {
				err = errors.New("pending write has no durable turn operation")
			} else {
				id, err = write.applyTurn(request.ctx, request.durable, request.turnID, leaf)
			}
		} else if write.applyStore != nil {
			if request.store == nil {
				err = errors.New("session store is unavailable")
			} else {
				err = write.applyStore(request.ctx, request.store)
			}
		} else if write.apply != nil {
			if request.sess == nil {
				err = errors.New("session is unavailable")
			} else {
				err = write.apply(request.ctx, request.sess)
			}
		} else {
			err = errors.New("pending write has no operation")
		}
		if err != nil {
			result.failedIndex = i
			result.err = fmt.Errorf("flush pending write: %w", err)
			result.leafID = leaf
			return result
		}
		result.succeeded++
		if id != "" {
			leaf = id
		}
	}
	result.leafID = leaf
	return result
}

func contextWithSignalAndRelease(parent context.Context, signal <-chan struct{}) (context.Context, func()) {
	if signal == nil {
		return parent, func() {}
	}
	ctx, cancel := context.WithCancel(parent)
	done := make(chan struct{})
	go func() {
		defer close(done)
		select {
		case <-signal:
			cancel()
		case <-ctx.Done():
		}
	}()
	return ctx, func() {
		cancel()
		<-done
	}
}

func minInt(a, b int) int {
	if a < b {
		return a
	}
	return b
}
