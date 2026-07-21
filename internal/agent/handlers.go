package agent

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"

	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Handler methods for typed commands. Each handler runs on the command
// goroutine (the single writer). Phase validation happens before any I/O.
// Long-running operations (provider calls, persistence) spawn a worker
// goroutine so the command loop is not blocked.

// --- Turn commands ---

func (c *Controller) handlePrompt(cmd *PromptCmd) {
	runDone, err := c.beginTurn(cmd.Ctx)
	if err != nil {
		sendResult(cmd.Reply, PromptResult{Err: turnError(KindInternal, c.currentPhase(), RecoveryNone, err)})
		return
	}

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
	steer, followUp, err := c.cancelActiveRun()
	if err != nil {
		sendResult(cmd.Reply, AbortResult{Err: turnError(KindCancellation, c.currentPhase(), RecoveryNone, err)})
		return
	}
	sendResult(cmd.Reply, AbortResult{Steer: steer, FollowUp: followUp})
}

// --- Model and tool commands ---

func (c *Controller) handleSetModel(cmd *SetModelCmd) {
	sendResult(cmd.Reply, c.setModelDirect(cmd.Model))
}

func (c *Controller) handleSetThinking(cmd *SetThinkingCmd) {
	c.startOperation(func() {
		sendResult(cmd.Reply, c.setThinkingDirect(cmd.Ctx, cmd.Level))
	})
}

func (c *Controller) handleSetTools(cmd *SetToolsCmd) {
	sendResult(cmd.Reply, c.setToolsDirect(cmd.Tools, cmd.Active))
}

func (c *Controller) handleActivateTools(cmd *ActivateToolsCmd) {
	c.startOperation(func() {
		sendResult(cmd.Reply, c.activateToolsDirect(cmd.Ctx, cmd.Names))
	})
}

// --- Session administration commands ---

func (c *Controller) handleSubscribe(cmd *SubscribeCmd) {
	c.startOperation(func() {
		sub, err := c.subscribeDirect(cmd.Ctx, cmd.After)
		sendResult(cmd.Reply, SubscribeResult{Sub: sub, Err: err})
	})
}

func (c *Controller) handleCompact(cmd *CompactCmd) {
	finish, err := c.beginExclusive(PhasePersisting)
	if err != nil {
		sendResult(cmd.Reply, err)
		return
	}
	c.startOperation(func() {
		defer finish()
		result := c.requestRuntime(cmd.Ctx, runtimeRequest{kind: runtimeCompact, force: true})
		sendResult(cmd.Reply, result.err)
	})
}

func (c *Controller) handleNavigate(cmd *NavigateCmd) {
	c.startOperation(func() {
		result, err := c.navigateTreeDirect(cmd.Ctx, cmd.Target, cmd.Opts)
		sendResult(cmd.Reply, NavigateCmdResult{Result: result, Err: err})
	})
}

func (c *Controller) handleAppendSessionInfo(cmd *AppendSessionInfoCmd) {
	c.startOperation(func() {
		name, err := c.appendSessionInfoDirect(cmd.Ctx, cmd.Name)
		sendResult(cmd.Reply, SessionInfoResult{Name: name, Err: err})
	})
}

func (c *Controller) handleAppendLabel(cmd *AppendLabelCmd) {
	c.startOperation(func() {
		name, err := c.appendLabelDirect(cmd.Ctx, cmd.Target, cmd.Label)
		sendResult(cmd.Reply, SessionInfoResult{Name: name, Err: err})
	})
}

func (c *Controller) handleGetLabel(cmd *GetLabelCmd) {
	c.startOperation(func() {
		name, err := c.getLabelDirect(cmd.Ctx, cmd.Target)
		sendResult(cmd.Reply, SessionInfoResult{Name: name, Err: err})
	})
}

func (c *Controller) handleAppendMessage(cmd *AppendMessageCmd) {
	c.startOperation(func() {
		sendResult(cmd.Reply, c.appendMessageDirect(cmd.Ctx, cmd.Message))
	})
}

func (c *Controller) handlePersistEntry(cmd *PersistEntryCmd) {
	c.startOperation(func() {
		sendResult(cmd.Reply, c.persistEntryDirect(cmd.Ctx, cmd.Entry))
	})
}

func (c *Controller) handleForkSession(cmd *ForkSessionCmd) {
	c.startOperation(func() {
		id, err := c.forkSessionDirect(cmd.Ctx, cmd.SourceID)
		sendResult(cmd.Reply, ForkResult{ID: id, Err: err})
	})
}

// --- Approval commands ---

// ResolveApproval supplies the host's decision for a pending tool call.
// This is a direct broker call — it does not go through the command queue
// because the approval broker is already goroutine-safe and the decision
// must be delivered immediately without queueing latency.
func (c *Controller) ResolveApproval(id string, decision session.ApprovalDecision) error {
	if c == nil || c.approvals == nil {
		return errors.New("approval broker is unavailable")
	}
	return c.approvals.Resolve(id, decision)
}

// --- Capability methods (non-Runtime interfaces) ---

// Session returns the active session for read-only projections.
func (c *Controller) Session() session.Session {
	return c.session
}

// Store returns the session store.
func (c *Controller) Store() session.Store {
	return c.store
}

// CloseResources closes host-created resources after the controller has
// stopped. Called by the composition root, not by Runtime.Close.
func (c *Controller) CloseResources() error {
	var firstErr error
	c.resourcesOnce.Do(func() {
		for _, fn := range c.closeResources {
			if err := fn(); err != nil && firstErr == nil {
				firstErr = err
			}
		}
	})
	return firstErr
}

// Shutdown stops the controller with a context-bounded wait for active
// work, then closes resources.
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

	if err := c.flushPending(context.Background()); err != nil {
		c.logf(slog.LevelError, "shutdown pending write failed", slog.String("error", err.Error()))
		return errors.Join(err, c.Close())
	}
	c.logf(slog.LevelInfo, "shutdown complete")
	return c.Close()
}

// UnsettledActions returns externally observable actions whose outcome was
// not proven. This is a direct store read — it does not go through the
// command queue because it is read-only and the journal is goroutine-safe.
func (c *Controller) UnsettledActions(ctx context.Context) ([]session.ActionRecord, error) {
	journal, ok := c.store.(session.ActionJournal)
	if !ok {
		return nil, errors.New("session store does not support action recovery")
	}
	return journal.UnsettledActions(ctx)
}

// ReconcileAction records the externally observed outcome of an action.
func (c *Controller) ReconcileAction(ctx context.Context, actionID string, state session.ActionState, verification, resultIdentity, reason, cleanup string) (session.ActionRecord, error) {
	journal, ok := c.store.(session.ActionJournal)
	if !ok {
		return session.ActionRecord{}, errors.New("session store does not support action recovery")
	}
	return journal.ReconcileAction(ctx, actionID, state, verification, resultIdentity, reason, cleanup)
}

// ExportSessionBundle exports a session as a bundle.
func (c *Controller) ExportSessionBundle(ctx context.Context, sessionID string) (ionexport.SessionBundle, error) {
	return c.exportSessionBundleDirect(ctx, sessionID)
}

// ImportSessionBundle imports a session bundle.
func (c *Controller) ImportSessionBundle(ctx context.Context, bundle ionexport.SessionBundle) (string, error) {
	return c.importSessionBundleDirect(ctx, bundle)
}

// ForkSession creates a new session rooted at a source.
func (c *Controller) ForkSession(ctx context.Context, sourceID string) (string, error) {
	reply := make(chan ForkResult, 1)
	cmd := &ForkSessionCmd{Ctx: ctx, SourceID: sourceID, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.ID, result.Err
}

// Compact requests context compaction at a safe boundary.
func (c *Controller) Compact(ctx context.Context) error {
	reply := make(chan error, 1)
	cmd := &CompactCmd{Ctx: ctx, Reply: reply}
	return c.enqueueSync(ctx, cmd, reply)
}

// NavigateTree moves the active session leaf.
func (c *Controller) NavigateTree(ctx context.Context, targetID string, opts NavigateOptions) (NavigateResult, error) {
	reply := make(chan NavigateCmdResult, 1)
	cmd := &NavigateCmd{Ctx: ctx, Target: targetID, Opts: opts, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return NavigateResult{}, err
	}
	result := <-reply
	return result.Result, result.Err
}

// AppendSessionInfo updates display metadata.
func (c *Controller) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	reply := make(chan SessionInfoResult, 1)
	cmd := &AppendSessionInfoCmd{Ctx: ctx, Name: name, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.Name, result.Err
}

// AppendLabel writes a branch label.
func (c *Controller) AppendLabel(ctx context.Context, targetID, label string) (string, error) {
	reply := make(chan SessionInfoResult, 1)
	cmd := &AppendLabelCmd{Ctx: ctx, Target: targetID, Label: label, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.Name, result.Err
}

// GetLabel reads a branch label.
func (c *Controller) GetLabel(ctx context.Context, targetID string) (string, error) {
	reply := make(chan SessionInfoResult, 1)
	cmd := &GetLabelCmd{Ctx: ctx, Target: targetID, Reply: reply}
	if err := c.enqueue(ctx, cmd); err != nil {
		return "", err
	}
	result := <-reply
	return result.Name, result.Err
}

// AppendMessage appends a message directly.
func (c *Controller) AppendMessage(ctx context.Context, msg session.Message) error {
	reply := make(chan error, 1)
	cmd := &AppendMessageCmd{Ctx: ctx, Message: msg, Reply: reply}
	return c.enqueueSync(ctx, cmd, reply)
}

// PersistEntry persists a non-turn entry.
func (c *Controller) PersistEntry(ctx context.Context, entry session.Entry) error {
	reply := make(chan error, 1)
	cmd := &PersistEntryCmd{Ctx: ctx, Entry: entry, Reply: reply}
	return c.enqueueSync(ctx, cmd, reply)
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

// formatPhaseError wraps an error with phase context.
func formatPhaseError(phase Phase, err error) error {
	if err == nil {
		return nil
	}
	return fmt.Errorf("phase %s: %w", phase, err)
}
