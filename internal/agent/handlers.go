package agent

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"time"

	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// Handler methods for typed commands. Each handler runs on the command
// goroutine (the single writer). Phase validation happens before any I/O.
// Long-running operations (provider calls, persistence) spawn a worker
// goroutine so the command loop is not blocked.

// --- Turn commands ---

func (c *Controller) handlePrompt(cmd *PromptCmd) {
	if err := cmd.Ctx.Err(); err != nil {
		if cmd.TurnToken != 0 {
			_, _, _ = c.cancelActiveRun(cmd.TurnToken)
			c.releaseTurnToken(cmd.TurnToken)
		}
		sendResult(cmd.Reply, PromptResult{Err: turnError(KindCancellation, c.currentPhase(), RecoveryNone, err)})
		return
	}
	runDone, err := c.beginTurn(cmd.Ctx, cmd.TurnToken)
	if err != nil {
		c.releaseTurnToken(cmd.TurnToken)
		kind := KindInternal
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			kind = KindCancellation
		}
		sendResult(cmd.Reply, PromptResult{Err: turnError(kind, c.currentPhase(), RecoveryNone, err)})
		return
	}
	if sink := TurnAcceptanceSinkFromContext(cmd.Ctx); sink != nil {
		sink()
	}
	c.startPromptWorker(cmd, runDone)
}

func (c *Controller) startPromptWorker(cmd *PromptCmd, runDone chan struct{}) {
	go func() {
		defer c.turnWorkers.Done()
		c.mu.Lock()
		turnContext := c.runContext
		c.mu.Unlock()
		if turnContext == nil {
			turnContext = cmd.Ctx
		}
		var (
			msg    session.Message
			runErr error
		)
		func() {
			defer func() {
				if recovered := recover(); recovered != nil {
					runErr = fmt.Errorf("turn worker panic: %v", recovered)
				}
			}()
			msg, runErr = c.runPrompt(turnContext, cmd.Text, cmd.Images...)
		}()
		completion := turnCompletion{
			runDone: runDone,
			reply:   cmd.Reply,
			message: msg,
			runErr:  runErr,
			ack:     make(chan struct{}),
		}
		c.completions <- completion
		<-completion.ack
	}()
}

// handleTurnCompletion is the single lifecycle finalizer for prompt workers.
// It converts the raw worker error into the public typed result, closes the
// turn wait channel, and only then acknowledges the worker.
func (c *Controller) handleTurnCompletion(completion turnCompletion) {
	c.mu.Lock()
	result := PromptResult{Message: completion.message}
	if completion.runErr != nil {
		var turnErr *TurnError
		if errors.As(completion.runErr, &turnErr) {
			result.Err = turnErr
		} else {
			result.Err = turnError(KindProvider, c.phase, RecoveryAbort, completion.runErr)
		}
	}
	if c.runDone == completion.runDone {
		if c.closed {
			if c.phase != PhaseClosed {
				_ = c.transitionPhase(c.phase, PhaseClosed)
			}
		} else {
			if completion.runErr != nil && c.phase == PhaseStreaming {
				_ = c.transitionPhase(PhaseStreaming, PhaseRecovering)
			} else if c.phase == PhaseStreaming {
				_ = c.transitionPhase(PhaseStreaming, PhasePersisting)
			}
			if c.phase == PhasePersisting || c.phase == PhaseRecovering {
				_ = c.transitionPhase(c.phase, PhaseSettled)
			}
			if c.phase == PhaseSettled {
				_ = c.transitionPhase(PhaseSettled, PhaseReady)
			}
		}
		c.activeTurnID = ""
		c.activeTurnLeaf = ""
		c.activeTurnToken = 0
		c.turnCommitted = false
		c.turnAborted = false
		c.runCancel = nil
		c.runCancelOnce = nil
		if c.runContextCancel != nil {
			c.runContextCancel()
		}
		c.runContext = nil
		c.runContextCancel = nil
		close(completion.runDone)
		c.runDone = nil
	}
	c.mu.Unlock()

	sendResult(completion.reply, result)
	close(completion.ack)
	c.startNextTurnIfReady()
}

func (c *Controller) handleSteer(cmd *SteerCmd) {
	phase := c.currentPhase()
	if !phase.activeTurn() {
		sendResult(cmd.Reply, ErrNoActiveTurn)
		return
	}
	sendResult(cmd.Reply, c.steerDirect(cmd.Text, cmd.Images...))
}

func (c *Controller) handleFollowUp(cmd *FollowUpCmd) {
	phase := c.currentPhase()
	if !phase.activeTurn() {
		sendResult(cmd.Reply, ErrNoActiveTurn)
		return
	}
	sendResult(cmd.Reply, c.followUpDirect(cmd.Text, cmd.Images...))
}

func (c *Controller) handleNextTurn(cmd *NextTurnCmd) {
	sendResult(cmd.Reply, c.nextTurnDirect(cmd.Text, cmd.Images...))
}

func (c *Controller) handleAbort(cmd *AbortCmd) {
	steer, followUp, err := c.cancelActiveRun(cmd.ExpectedTurnToken)
	if err != nil {
		sendResult(cmd.Reply, AbortResult{Err: turnError(KindCancellation, c.currentPhase(), RecoveryNone, err)})
		return
	}
	sendResult(cmd.Reply, AbortResult{Steer: steer, FollowUp: followUp})
}

// --- Model and tool commands ---

func (c *Controller) handleSetModel(cmd *SetModelCmd) {
	c.startOperation(func() {
		sendResult(cmd.Reply, c.setModelDirect(cmd.Model))
	})
}

func (c *Controller) handleSetThinking(cmd *SetThinkingCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		leafID, err := c.setThinkingDirect(ctx, cmd.Level)
		sendResult(cmd.Reply, ThinkingResult{LeafID: leafID, Err: err})
	})
}

func (c *Controller) handleSetTools(cmd *SetToolsCmd) {
	c.startOperation(func() {
		sendResult(cmd.Reply, c.setToolsDirect(cmd.Tools, cmd.Active))
	})
}

func (c *Controller) handleActivateTools(cmd *ActivateToolsCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		sendResult(cmd.Reply, c.activateToolsDirect(ctx, cmd.Names))
	})
}

// --- Action commands ---

// Action transitions are accepted by the controller and performed by joined
// operation workers. This keeps journal and approval I/O off the command loop
// while preserving one runtime-owned entry point for every transition.
func (c *Controller) handlePrepareAction(cmd *PrepareActionCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		coordinator, err := c.actionCoordinator()
		if err != nil {
			sendResult(cmd.Reply, ActionPrepareResult{Err: err})
			return
		}
		token, err := coordinator.prepareAndAuthorizeDirect(ctx, cmd.Request)
		sendResult(cmd.Reply, ActionPrepareResult{Token: token, Err: err})
	})
}

func (c *Controller) handleStartAction(cmd *StartActionCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		coordinator, err := c.actionCoordinator()
		if err != nil {
			sendResult(cmd.Reply, ActionStartResult{Err: err})
			return
		}
		token := cmd.Token
		err = coordinator.startDirect(ctx, &token, cmd.ProcessIdentity)
		sendResult(cmd.Reply, ActionStartResult{Token: &token, Err: err})
	})
}

func (c *Controller) handleFinishAction(cmd *FinishActionCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		coordinator, err := c.actionCoordinator()
		if err == nil {
			token := cmd.Token
			err = coordinator.finishDirect(ctx, &token, cmd.Result)
		}
		sendResult(cmd.Reply, err)
	})
}

func (c *Controller) handleCancelAction(cmd *CancelActionCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		coordinator, err := c.actionCoordinator()
		if err == nil {
			token := cmd.Token
			err = coordinator.cancelDirect(ctx, &token, cmd.Reason)
		}
		sendResult(cmd.Reply, err)
	})
}

func (c *Controller) handleUnsettledActions(cmd *UnsettledActionsCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		journal, err := c.actionJournal()
		if err != nil {
			sendResult(cmd.Reply, UnsettledActionsResult{Err: err})
			return
		}
		actions, err := journal.UnsettledActions(ctx)
		sendResult(cmd.Reply, UnsettledActionsResult{Actions: actions, Err: err})
	})
}

func (c *Controller) handleReconcileAction(cmd *ReconcileActionCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		journal, err := c.actionJournal()
		if err == nil {
			var action session.ActionRecord
			action, err = journal.ReconcileAction(
				ctx, cmd.ActionID, cmd.State, cmd.Verification,
				cmd.ResultIdentity, cmd.Reason, cmd.Cleanup,
			)
			sendResult(cmd.Reply, ReconcileActionResult{Action: action, Err: err})
			return
		}
		sendResult(cmd.Reply, ReconcileActionResult{Err: err})
	})
}

func (c *Controller) handleRecoverProcessActions(cmd *RecoverProcessActionsCmd) {
	c.startOperation(func() {
		// Recovery must remain durable after caller cancellation so a completed
		// reconciliation is not lost, but both the reconciler and its durable
		// writes must stop when the runtime closes. Keep those lifetimes separate.
		operationCtx, releaseOperation := c.runtimeBoundContext(cmd.Ctx)
		defer releaseOperation()
		durableRuntimeCtx, releaseDurableRuntime := c.runtimeBoundContext(context.Background())
		defer releaseDurableRuntime()

		journal, err := c.actionJournal()
		if err != nil {
			sendResult(cmd.Reply, err)
			return
		}
		actions, err := journal.UnsettledActions(operationCtx)
		if err != nil {
			sendResult(cmd.Reply, err)
			return
		}
		durableCtx, cancelDurability := context.WithTimeout(durableRuntimeCtx, 5*time.Second)
		defer cancelDurability()
		for _, action := range actions {
			if action.State == session.ActionStarted {
				action, err = journal.FinishAction(
					durableCtx,
					action.ID,
					session.ActionIndeterminate,
					"",
					"startup found an action after its durable start boundary",
					"startup recovery required",
				)
				if err != nil {
					sendResult(cmd.Reply, err)
					return
				}
			}
			if action.State != session.ActionIndeterminate {
				continue
			}
			result := tool.ProcessRecoveryResult{
				Status: tool.ProcessRecoveryUnavailable,
				Detail: "no process identity was durably recorded; manual verification is required",
			}
			if action.ProcessIdentity != "" {
				if c.processReconciler == nil {
					result.Detail = "runtime has no process reconciler; manual verification is required"
				} else {
					result, err = c.processReconciler.ReconcileProcess(operationCtx, action.ProcessIdentity)
					if err != nil {
						result = tool.ProcessRecoveryResult{
							Status: tool.ProcessRecoveryFailed,
							Detail: fmt.Sprintf("process reconciler failed: %v", err),
						}
					}
				}
			}
			if result.Status == "" {
				result.Status = tool.ProcessRecoveryFailed
				result.Detail = "process reconciler returned no recovery status"
			}
			if result.Detail == "" {
				result.Detail = string(result.Status)
			}
			reason := "restart process recovery: " + result.Detail
			cleanup := "restart process recovery status: " + string(result.Status)
			if _, err := journal.RecordActionRecovery(durableCtx, action.ID, reason, cleanup); err != nil {
				sendResult(cmd.Reply, err)
				return
			}
		}
		sendResult(cmd.Reply, nil)
	})
}

func (c *Controller) handleInterruptedTurns(cmd *InterruptedTurnsCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		if c.durable == nil {
			sendResult(cmd.Reply, InterruptedTurnsResult{Err: errors.New("runtime does not support durable turns")})
			return
		}
		turns, err := c.durable.InterruptedTurns(ctx)
		sendResult(cmd.Reply, InterruptedTurnsResult{Turns: turns, Err: err})
	})
}

func (c *Controller) handleAbortInterruptedTurn(cmd *AbortInterruptedTurnCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		if c.durable == nil {
			sendResult(cmd.Reply, AbortInterruptedTurnResult{Err: errors.New("runtime does not support durable turns")})
			return
		}
		turnID := strings.TrimSpace(cmd.TurnID)
		if turnID == "" {
			sendResult(cmd.Reply, AbortInterruptedTurnResult{Err: errors.New("turn ID is required")})
			return
		}
		record, err := c.durable.GetTurn(ctx, turnID)
		if err != nil {
			sendResult(cmd.Reply, AbortInterruptedTurnResult{Err: err})
			return
		}
		if record.State != session.TurnInterrupted {
			sendResult(cmd.Reply, AbortInterruptedTurnResult{
				Err: fmt.Errorf("turn %q is %s; only interrupted turns can be discarded", turnID, record.State),
			})
			return
		}
		if err := c.durable.AbortTurn(ctx, turnID, strings.TrimSpace(cmd.Reason)); err != nil {
			sendResult(cmd.Reply, AbortInterruptedTurnResult{Err: err})
			return
		}
		record, err = c.durable.GetTurn(ctx, turnID)
		sendResult(cmd.Reply, AbortInterruptedTurnResult{Turn: record, Err: err})
	})
}

// --- Session administration commands ---

func (c *Controller) handleSubscribe(cmd *SubscribeCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		sub, err := c.subscribeDirect(ctx, cmd.After)
		sendResult(cmd.Reply, SubscribeResult{Sub: sub, Err: err})
	})
}

func (c *Controller) handleCompact(cmd *CompactCmd) {
	finish, err := c.beginExclusive(PhasePersisting)
	if err != nil {
		sendResult(cmd.Reply, err)
		return
	}
	compactCtx, compactCancel := context.WithCancel(commandContext(cmd.Ctx))
	c.mu.Lock()
	c.nextCompactionCancelToken++
	token := c.nextCompactionCancelToken
	c.compactionCancelToken = token
	c.compactionCancel = compactCancel
	c.mu.Unlock()
	if err := c.startReservedOperation(finish, func() {
		result := c.requestRuntime(compactCtx, runtimeRequest{kind: runtimeCompact, force: true})
		compactCancel()
		c.mu.Lock()
		if c.compactionCancelToken == token {
			c.compactionCancel = nil
			c.compactionCancelToken = 0
		}
		c.mu.Unlock()
		finish()
		sendResult(cmd.Reply, result.err)
	}); err != nil {
		compactCancel()
		c.mu.Lock()
		if c.compactionCancelToken == token {
			c.compactionCancel = nil
			c.compactionCancelToken = 0
		}
		c.mu.Unlock()
		sendResult(cmd.Reply, err)
	}
}

func (c *Controller) handleNavigate(cmd *NavigateCmd) {
	finish, err := c.beginExclusive(PhaseRecovering)
	if err != nil {
		sendResult(cmd.Reply, NavigateCmdResult{Err: err})
		return
	}
	if err := c.startReservedOperation(finish, func() {
		result, err := c.navigateTreeDirect(cmd.Ctx, cmd.Target, cmd.Opts)
		finish()
		sendResult(cmd.Reply, NavigateCmdResult{Result: result, Err: err})
	}); err != nil {
		sendResult(cmd.Reply, NavigateCmdResult{Err: err})
	}
}

func (c *Controller) handleAppendSessionInfo(cmd *AppendSessionInfoCmd) {
	finish, err := c.beginExclusive(PhasePersisting)
	if err != nil {
		sendResult(cmd.Reply, SessionInfoResult{Err: err})
		return
	}
	if err := c.startReservedOperation(finish, func() {
		name, err := c.appendSessionInfoDirect(cmd.Ctx, cmd.ExpectedLeafID, cmd.Name)
		finish()
		sendResult(cmd.Reply, SessionInfoResult{Name: name, Err: err})
	}); err != nil {
		sendResult(cmd.Reply, SessionInfoResult{Err: err})
	}
}

func (c *Controller) handleAppendLabel(cmd *AppendLabelCmd) {
	finish, err := c.beginExclusive(PhasePersisting)
	if err != nil {
		sendResult(cmd.Reply, SessionInfoResult{Err: err})
		return
	}
	if err := c.startReservedOperation(finish, func() {
		name, err := c.appendLabelDirect(cmd.Ctx, cmd.ExpectedLeafID, cmd.Target, cmd.Label)
		finish()
		sendResult(cmd.Reply, SessionInfoResult{Name: name, Err: err})
	}); err != nil {
		sendResult(cmd.Reply, SessionInfoResult{Err: err})
	}
}

func (c *Controller) handleGetLabel(cmd *GetLabelCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		name, err := c.getLabelDirect(ctx, cmd.Target)
		sendResult(cmd.Reply, SessionInfoResult{Name: name, Err: err})
	})
}

func (c *Controller) handleGetBranchLabel(cmd *GetBranchLabelCmd) {
	finish, err := c.beginExclusive(PhasePersisting)
	if err != nil {
		sendResult(cmd.Reply, SessionInfoResult{Err: err})
		return
	}
	if err := c.startReservedOperation(finish, func() {
		label, err := c.getBranchLabelDirect(cmd.Ctx, cmd.LeafID)
		finish()
		sendResult(cmd.Reply, SessionInfoResult{Name: label, Err: err})
	}); err != nil {
		sendResult(cmd.Reply, SessionInfoResult{Err: err})
	}
}

func (c *Controller) handleForkSession(cmd *ForkSessionCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		id, err := c.forkSessionDirect(ctx, cmd.SourceID)
		sendResult(cmd.Reply, ForkResult{ID: id, Err: err})
	})
}

func (c *Controller) handleExportSessionBundle(cmd *ExportSessionBundleCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		bundle, err := c.exportSessionBundleDirect(ctx, cmd.SessionID)
		sendResult(cmd.Reply, ExportSessionResult{Bundle: bundle, Err: err})
	})
}

func (c *Controller) handleImportSessionBundle(cmd *ImportSessionBundleCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		id, err := c.importSessionBundleDirect(ctx, cmd.Bundle)
		sendResult(cmd.Reply, ImportSessionResult{ID: id, Err: err})
	})
}

func (c *Controller) handleSessionProjection(cmd *SessionProjectionCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		projection, err := c.sessionProjectionDirect(ctx)
		sendResult(cmd.Reply, SessionProjectionResult{Projection: projection, Err: err})
	})
}

func (c *Controller) sessionProjectionDirect(ctx context.Context) (SessionProjection, error) {
	ctx = commandContext(ctx)
	c.mu.Lock()
	sess := c.session
	durable := c.durable
	turnID := c.activeTurnID
	c.mu.Unlock()
	if sess == nil {
		return SessionProjection{}, errors.New("session is unavailable")
	}

	if durable != nil && turnID != "" {
		entries, err := durable.TurnBranch(ctx, turnID)
		if err != nil {
			return SessionProjection{}, fmt.Errorf("read active turn branch: %w", err)
		}
		return newSessionProjection(sess.ID(), sess.GetLeafID(), sess.Meta().Branch, entries), nil
	}

	for attempt := 0; attempt < 3; attempt++ {
		leafID := sess.GetLeafID()
		entries, err := sess.BranchAt(ctx, leafID)
		if err != nil {
			return SessionProjection{}, err
		}
		if sess.GetLeafID() == leafID {
			return newSessionProjection(sess.ID(), leafID, sess.Meta().Branch, entries), nil
		}
		if err := ctx.Err(); err != nil {
			return SessionProjection{}, err
		}
	}
	return SessionProjection{}, ErrSessionTreeChanged
}

func newSessionProjection(id, leafID, worktreeBranch string, entries []session.Entry) SessionProjection {
	branch := append([]session.Entry(nil), entries...)
	if len(branch) > 0 {
		leafID = branch[len(branch)-1].ID()
	}
	return SessionProjection{
		ID:             id,
		LeafID:         leafID,
		Branch:         branch,
		Usage:          session.UsageFromEntries(branch),
		WorktreeBranch: strings.TrimSpace(worktreeBranch),
	}
}

func (c *Controller) handleSessionBranch(cmd *SessionBranchCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		if c.session == nil {
			sendResult(cmd.Reply, SessionBranchResult{Err: errors.New("session is unavailable")})
			return
		}
		entries, err := c.session.Branch(ctx)
		sendResult(cmd.Reply, SessionBranchResult{Entries: entries, Err: err})
	})
}

func (c *Controller) handleSessionBranchAt(cmd *SessionBranchAtCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		if c.session == nil {
			sendResult(cmd.Reply, SessionBranchResult{Err: errors.New("session is unavailable")})
			return
		}
		leafID := strings.TrimSpace(cmd.LeafID)
		if leafID == "" {
			sendResult(cmd.Reply, SessionBranchResult{Err: errors.New("session leaf is required")})
			return
		}
		entries, err := c.session.BranchAt(ctx, leafID)
		sendResult(cmd.Reply, SessionBranchResult{Entries: entries, Err: err})
	})
}

func (c *Controller) handleSessionTree(cmd *SessionTreeCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		if c.store == nil {
			sendResult(cmd.Reply, SessionTreeResult{Err: errors.New("session store is unavailable")})
			return
		}
		// GetLeafID and Entries are separate Store reads. A navigation can
		// commit between them, so only publish a pair after confirming that
		// the selected leaf remained stable. Never hand the picker a mixed
		// snapshot that can mark the wrong entry as current.
		for attempt := 0; attempt < 3; attempt++ {
			leafID := c.store.GetLeafID()
			entries, err := c.store.Entries(ctx)
			if err != nil {
				sendResult(cmd.Reply, SessionTreeResult{
					Tree: SessionTreeSnapshot{LeafID: leafID},
					Err:  err,
				})
				return
			}
			if c.store.GetLeafID() == leafID {
				sendResult(cmd.Reply, SessionTreeResult{
					Tree: SessionTreeSnapshot{LeafID: leafID, Entries: entries},
				})
				return
			}
			if err := ctx.Err(); err != nil {
				sendResult(cmd.Reply, SessionTreeResult{Err: err})
				return
			}
		}
		sendResult(cmd.Reply, SessionTreeResult{Err: ErrSessionTreeChanged})
	})
}

func (c *Controller) handleSessionCatalogList(cmd *SessionCatalogListCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		catalog, ok := c.store.(SessionCatalog)
		if !ok {
			sendResult(
				cmd.Reply,
				SessionCatalogListResult{Err: errors.New("session store does not support session catalog")},
			)
			return
		}
		sessions, err := catalog.ListSessions(ctx, cmd.Workdir)
		sendResult(cmd.Reply, SessionCatalogListResult{Sessions: sessions, Err: err})
	})
}

func (c *Controller) handleSessionCatalogLookup(cmd *SessionCatalogLookupCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		catalog, ok := c.store.(SessionCatalog)
		if !ok {
			sendResult(
				cmd.Reply,
				SessionCatalogLookupResult{Err: errors.New("session store does not support session catalog")},
			)
			return
		}
		info, err := catalog.GetSessionInfo(ctx, cmd.SessionID)
		sendResult(cmd.Reply, SessionCatalogLookupResult{Info: info, Err: err})
	})
}

func (c *Controller) handleSessionCatalogUpdate(cmd *SessionCatalogUpdateCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		catalog, ok := c.store.(SessionCatalog)
		if !ok {
			sendResult(cmd.Reply, errors.New("session store does not support session catalog"))
			return
		}
		sendResult(cmd.Reply, catalog.UpdateSession(ctx, cmd.Info))
	})
}

func (c *Controller) handleInputHistoryGet(cmd *InputHistoryGetCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		history, ok := c.store.(InputHistory)
		if !ok {
			sendResult(
				cmd.Reply,
				InputHistoryGetResult{Err: errors.New("session store does not support input history")},
			)
			return
		}
		inputs, err := history.GetInputs(ctx, cmd.Workdir, cmd.Limit)
		sendResult(cmd.Reply, InputHistoryGetResult{Inputs: inputs, Err: err})
	})
}

func (c *Controller) handleInputHistoryAdd(cmd *InputHistoryAddCmd) {
	c.startContextOperation(cmd.Ctx, func(ctx context.Context) {
		history, ok := c.store.(InputHistory)
		if !ok {
			sendResult(cmd.Reply, errors.New("session store does not support input history"))
			return
		}
		sendResult(cmd.Reply, history.AddInput(ctx, cmd.Workdir, cmd.Input))
	})
}

// --- Approval commands ---

// ResolveApproval supplies the host's decision for a pending tool call.
//
// Approval resolution is the runtime's synchronous control lane. It is a
// direct broker call, not a command-queue operation: the broker is owned by
// this controller, is goroutine-safe, and must wake a waiting tool
// immediately. Controller.Close closes the same lane first, so a close-vs-
// resolve race produces one terminal decision and never leaves a tool blocked.
func (c *Controller) ResolveApproval(id string, decision session.ApprovalDecision) error {
	if c == nil || c.approvals == nil {
		return errors.New("approval broker is unavailable")
	}
	return c.approvals.Resolve(id, decision)
}

// --- Capability methods (non-Runtime interfaces) ---

// SessionID returns the immutable identity of the active session. It does not
// expose the mutable session façade or storage owner.
func (c *Controller) SessionID() string {
	if c == nil || c.session == nil {
		return ""
	}
	return c.session.ID()
}

// SessionProjection reads the active session through the controller command
// queue. A durable active turn is projected from its staged branch; otherwise
// the selected leaf is captured and verified across the branch read.
func (c *Controller) SessionProjection(ctx context.Context) (SessionProjection, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionProjectionResult, 1)
	if err := c.enqueue(ctx, &SessionProjectionCmd{Ctx: ctx, Reply: reply}); err != nil {
		return SessionProjection{}, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return SessionProjection{}, err
	}
	return result.Projection, result.Err
}

// SessionBranch reads the active branch through the controller command queue.
func (c *Controller) SessionBranch(ctx context.Context) ([]session.Entry, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionBranchResult, 1)
	if err := c.enqueue(ctx, &SessionBranchCmd{Ctx: ctx, Reply: reply}); err != nil {
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	return result.Entries, result.Err
}

// SessionBranchAt reads a selected branch through the controller command
// queue. It is used for resume/model restoration before that leaf is active.
func (c *Controller) SessionBranchAt(ctx context.Context, leafID string) ([]session.Entry, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionBranchResult, 1)
	if err := c.enqueue(ctx, &SessionBranchAtCmd{Ctx: ctx, LeafID: leafID, Reply: reply}); err != nil {
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	return result.Entries, result.Err
}

// SessionTree reads the active tree through the controller command queue.
func (c *Controller) SessionTree(ctx context.Context) (SessionTreeSnapshot, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionTreeResult, 1)
	if err := c.enqueue(ctx, &SessionTreeCmd{Ctx: ctx, Reply: reply}); err != nil {
		return SessionTreeSnapshot{}, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return SessionTreeSnapshot{}, err
	}
	return result.Tree, result.Err
}

// ListSessions reads the session catalog through the controller command queue.
func (c *Controller) ListSessions(ctx context.Context, workdir string) ([]session.SessionInfoEntry, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionCatalogListResult, 1)
	if err := c.enqueue(ctx, &SessionCatalogListCmd{Ctx: ctx, Workdir: workdir, Reply: reply}); err != nil {
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	return result.Sessions, result.Err
}

// GetSessionInfo reads one session catalog entry through the controller.
func (c *Controller) GetSessionInfo(ctx context.Context, sessionID string) (session.SessionInfoEntry, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionCatalogLookupResult, 1)
	if err := c.enqueue(ctx, &SessionCatalogLookupCmd{Ctx: ctx, SessionID: sessionID, Reply: reply}); err != nil {
		return session.SessionInfoEntry{}, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return session.SessionInfoEntry{}, err
	}
	return result.Info, result.Err
}

// UpdateSession persists catalog metadata through the controller.
func (c *Controller) UpdateSession(ctx context.Context, info session.SessionInfoEntry) error {
	ctx = commandContext(ctx)
	reply := make(chan error, 1)
	return c.enqueueSync(ctx, &SessionCatalogUpdateCmd{Ctx: ctx, Info: info, Reply: reply}, reply)
}

// GetInputs reads bounded composer history through the controller.
func (c *Controller) GetInputs(ctx context.Context, workdir string, limit int) ([]string, error) {
	ctx = commandContext(ctx)
	reply := make(chan InputHistoryGetResult, 1)
	if err := c.enqueue(ctx, &InputHistoryGetCmd{Ctx: ctx, Workdir: workdir, Limit: limit, Reply: reply}); err != nil {
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	return result.Inputs, result.Err
}

// AddInput appends one composer-history item through the controller.
func (c *Controller) AddInput(ctx context.Context, workdir, input string) error {
	ctx = commandContext(ctx)
	reply := make(chan error, 1)
	return c.enqueueSync(ctx, &InputHistoryAddCmd{Ctx: ctx, Workdir: workdir, Input: input, Reply: reply}, reply)
}

// CloseResources closes host-created resources after the controller has
// stopped. Called by the composition root, not by Runtime.Close.
func (c *Controller) CloseResources() error {
	c.resourcesOnce.Do(func() {
		var errs []error
		for _, fn := range c.closeResources {
			if fn == nil {
				continue
			}
			if err := fn(); err != nil {
				errs = append(errs, err)
			}
		}
		c.resourcesErr = errors.Join(errs...)
	})
	return c.resourcesErr
}

// closeWithContext starts the terminal controller close and bounds how long
// the caller waits. Close itself remains joinable: a later Close waits for the
// same terminal cleanup instead of returning before the first close finishes.
func (c *Controller) closeWithContext(ctx context.Context) error {
	ctx = commandContext(ctx)
	done := make(chan error, 1)
	go func() { done <- c.Close() }()
	select {
	case err := <-done:
		return err
	case <-ctx.Done():
		return ctx.Err()
	}
}

// Shutdown stops the controller with a context-bounded wait for active work.
// Host-created resources remain the composition root's responsibility through
// CloseResources after the controller boundary has closed. If the shutdown
// deadline expires during pending durability, Shutdown returns while the
// runtime-owned flush remains joined; the caller must Close after any needed
// dependency release.
func (c *Controller) Shutdown(ctx context.Context) error {
	if ctx == nil {
		ctx = context.Background()
	}
	c.logf(slog.LevelInfo, "shutdown start")
	if _, _, err := c.cancelActiveRun(); err != nil {
		return err
	}
	c.emitQueueUpdate()

	c.mu.Lock()
	done := c.runDone
	c.mu.Unlock()
	if done != nil {
		select {
		case <-done:
		case <-ctx.Done():
			c.logf(slog.LevelWarn, "shutdown timed out waiting for turn")
			return ctx.Err()
		}
	}

	if err := c.flushPendingForShutdown(ctx); err != nil {
		if errors.Is(err, ctx.Err()) {
			c.logf(slog.LevelWarn, "shutdown timed out waiting for pending writes")
			return err
		}
		c.logf(slog.LevelError, "shutdown pending write failed", slog.String("error", err.Error()))
		if closeErr := c.closeWithContext(ctx); closeErr != nil {
			return errors.Join(err, closeErr)
		}
		return err
	}
	c.logf(slog.LevelInfo, "shutdown complete")
	return c.closeWithContext(ctx)
}

// RecoverProcessActions reconciles durable process evidence through the
// controller-owned recovery boundary without resolving the external outcome.
func (c *Controller) RecoverProcessActions(ctx context.Context) error {
	ctx = commandContext(ctx)
	reply := make(chan error, 1)
	if err := c.enqueue(ctx, &RecoverProcessActionsCmd{Ctx: ctx, Reply: reply}); err != nil {
		return err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return err
	}
	return result
}

// UnsettledActions returns externally observable actions whose outcome was
// not proven through the controller-owned recovery boundary.
func (c *Controller) UnsettledActions(ctx context.Context) ([]session.ActionRecord, error) {
	ctx = commandContext(ctx)
	reply := make(chan UnsettledActionsResult, 1)
	if err := c.enqueue(ctx, &UnsettledActionsCmd{Ctx: ctx, Reply: reply}); err != nil {
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	return result.Actions, result.Err
}

// InterruptedTurns returns durable turn records recovered from an interrupted
// process through the runtime-owned storage boundary.
func (c *Controller) InterruptedTurns(ctx context.Context) ([]session.TurnRecord, error) {
	ctx = commandContext(ctx)
	reply := make(chan InterruptedTurnsResult, 1)
	if err := c.enqueue(ctx, &InterruptedTurnsCmd{Ctx: ctx, Reply: reply}); err != nil {
		return nil, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return nil, err
	}
	return result.Turns, result.Err
}

// AbortInterruptedTurn explicitly settles one startup-recovered turn. It does
// not publish that turn's input or staged entries into conversation history.
func (c *Controller) AbortInterruptedTurn(ctx context.Context, turnID, reason string) (session.TurnRecord, error) {
	ctx = commandContext(ctx)
	reply := make(chan AbortInterruptedTurnResult, 1)
	if err := c.enqueue(ctx, &AbortInterruptedTurnCmd{
		Ctx: ctx, TurnID: turnID, Reason: reason, Reply: reply,
	}); err != nil {
		return session.TurnRecord{}, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return session.TurnRecord{}, err
	}
	return result.Turn, result.Err
}

// ReconcileAction records the externally observed outcome of an action.
func (c *Controller) ReconcileAction(
	ctx context.Context,
	actionID string,
	state session.ActionState,
	verification, resultIdentity, reason, cleanup string,
) (session.ActionRecord, error) {
	ctx = commandContext(ctx)
	reply := make(chan ReconcileActionResult, 1)
	if err := c.enqueue(ctx, &ReconcileActionCmd{
		Ctx: ctx, ActionID: actionID, State: state, Verification: verification,
		ResultIdentity: resultIdentity, Reason: reason, Cleanup: cleanup, Reply: reply,
	}); err != nil {
		return session.ActionRecord{}, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return session.ActionRecord{}, err
	}
	return result.Action, result.Err
}

// ExportSessionBundle exports a session as a bundle.
func (c *Controller) ExportSessionBundle(ctx context.Context, sessionID string) (ionexport.SessionBundle, error) {
	ctx = commandContext(ctx)
	reply := make(chan ExportSessionResult, 1)
	cmd := &ExportSessionBundleCmd{Ctx: ctx, SessionID: sessionID, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return ionexport.SessionBundle{}, err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return ionexport.SessionBundle{}, err
	}
	return result.Bundle, result.Err
}

// ImportSessionBundle imports a session bundle.
func (c *Controller) ImportSessionBundle(ctx context.Context, bundle ionexport.SessionBundle) (string, error) {
	ctx = commandContext(ctx)
	reply := make(chan ImportSessionResult, 1)
	cmd := &ImportSessionBundleCmd{Ctx: ctx, Bundle: bundle, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return "", err
	}
	return result.ID, result.Err
}

// ForkSession creates a new session rooted at a source.
func (c *Controller) ForkSession(ctx context.Context, sourceID string) (string, error) {
	ctx = commandContext(ctx)
	reply := make(chan ForkResult, 1)
	cmd := &ForkSessionCmd{Ctx: ctx, SourceID: sourceID, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return "", err
	}
	return result.ID, result.Err
}

// Compact requests context compaction at a safe boundary.
func (c *Controller) Compact(ctx context.Context) error {
	ctx = commandContext(ctx)
	reply := make(chan error, 1)
	cmd := &CompactCmd{Ctx: ctx, Reply: reply}
	// Compaction owns the runtime busy barrier until its operation worker
	// acknowledges completion. Once accepted, wait for that authoritative
	// result rather than allowing caller cancellation to release the app gate
	// early.
	if err := c.enqueue(ctx, cmd); err != nil {
		return err
	}
	return <-reply
}

// NavigateTree moves the active session leaf.
func (c *Controller) NavigateTree(ctx context.Context, targetID string, opts NavigateOptions) (NavigateResult, error) {
	ctx = commandContext(ctx)
	reply := make(chan NavigateCmdResult, 1)
	cmd := &NavigateCmd{Ctx: ctx, Target: targetID, Opts: opts, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return NavigateResult{}, err
	}
	result := <-reply
	return result.Result, result.Err
}

// AppendSessionInfo updates display metadata for the expected active leaf.
func (c *Controller) AppendSessionInfo(ctx context.Context, expectedLeafID, name string) (string, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionInfoResult, 1)
	cmd := &AppendSessionInfoCmd{
		Ctx:            ctx,
		ExpectedLeafID: expectedLeafID,
		Name:           name,
		Reply:          reply,
	}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.Name, result.Err
}

// AppendLabel writes a branch label for the expected active leaf.
func (c *Controller) AppendLabel(
	ctx context.Context,
	expectedLeafID, targetID, label string,
) (string, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionInfoResult, 1)
	cmd := &AppendLabelCmd{
		Ctx:            ctx,
		ExpectedLeafID: expectedLeafID,
		Target:         targetID,
		Label:          label,
		Reply:          reply,
	}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.Name, result.Err
}

// GetLabel reads a branch label.
func (c *Controller) GetLabel(ctx context.Context, targetID string) (string, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionInfoResult, 1)
	cmd := &GetLabelCmd{Ctx: ctx, Target: targetID, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result, err := waitCommandReply(ctx, reply)
	if err != nil {
		return "", err
	}
	return result.Name, result.Err
}

// GetBranchLabel reads the latest label on the explicit branch leaf.
func (c *Controller) GetBranchLabel(ctx context.Context, leafID string) (string, error) {
	ctx = commandContext(ctx)
	reply := make(chan SessionInfoResult, 1)
	cmd := &GetBranchLabelCmd{Ctx: ctx, LeafID: leafID, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.Name, result.Err
}

// PromptFromTemplate renders a named template with the given variables.
// This is a direct string operation — it does not go through the command
// queue.
func (c *Controller) PromptFromTemplate(name string, data map[string]string) string {
	tmpl, ok := c.promptTemplates[name]
	if !ok {
		return ""
	}
	result := tmpl
	for k, v := range data {
		result = strings.ReplaceAll(result, "{{"+k+"}}", v)
	}
	return result
}

// GetModel returns the active model.
func (c *Controller) GetModel() llm.Model {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.model
}

// GetThinkingLevel returns the active thinking level.
func (c *Controller) GetThinkingLevel() session.ThinkingLevel {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.thinking
}

// GetTools returns the tool map and active names.
func (c *Controller) GetTools() (map[string]Tool, []string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	tools := make(map[string]Tool, len(c.tools))
	for k, v := range c.tools {
		tools[k] = v
	}
	return tools, c.active
}

// WaitForIdle blocks until no turn is active.
func (c *Controller) WaitForIdle() {
	c.mu.Lock()
	done := c.runDone
	c.mu.Unlock()
	if done != nil {
		<-done
	}
}

// Metrics returns the metrics collector.
func (c *Controller) Metrics() *Metrics {
	return c.metrics
}

// Done returns a channel closed when the controller loop exits.
func (c *Controller) Done() <-chan struct{} {
	return c.done
}

// IsIdle reports whether the controller is in an idle phase.
func (c *Controller) IsIdle() bool {
	phase := c.currentPhase()
	return phase == PhaseReady || phase == PhaseSettled
}
