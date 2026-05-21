package canto

import (
	"context"
	"fmt"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/llm"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (s *Session) SubmitTurn(ctx context.Context, input string) error {
	return s.backend.submitTurn(ctx, input)
}

func (b *Backend) submitTurn(ctx context.Context, input string) error {
	submitted, err := b.prepareSubmittedTurn(ctx, input)
	if err != nil {
		return err
	}

	turn, err := b.acceptSubmittedTurn(submitted, input)
	if err != nil {
		return err
	}

	b.wg.Go(func() {
		b.runTurn(submitted.ctx, submitted.id, submitted.cancel, turn)
	})

	return nil
}

type submittedTurn struct {
	id        uint64
	ctx       context.Context
	cancel    context.CancelFunc
	submitter turnSubmitter
}

func (b *Backend) prepareSubmittedTurn(
	ctx context.Context,
	input string,
) (submittedTurn, error) {
	b.mu.Lock()
	if b.harness == nil {
		b.mu.Unlock()
		return submittedTurn{}, fmt.Errorf("backend not initialized")
	}
	if b.turn.active {
		b.mu.Unlock()
		return submittedTurn{}, fmt.Errorf("turn already in progress")
	}
	sess := b.sess
	sessionID := b.idLocked()
	if sessionID == "" {
		sessionID = "default"
	}
	ionStore := b.ionStore
	modelName := storageModelName(b.Provider(), b.Model())
	turnCtx, cancel := context.WithCancel(ctx)
	turnID := b.turn.start(cancel)
	harnessSession := b.harness.Session(sessionID)
	b.mu.Unlock()

	abort := func(err error) (submittedTurn, error) {
		cancel()
		b.finishTurnIfActive(turnID)
		return submittedTurn{}, err
	}

	if lazy, ok := sess.(interface {
		Ensure(context.Context) (storage.Session, error)
	}); ok {
		materialized, err := lazy.Ensure(turnCtx)
		if err != nil {
			return abort(fmt.Errorf("open session: %w", err))
		}
		sess = materialized
		b.mu.Lock()
		if b.turn.activeFor(turnID) {
			b.sess = materialized
			sessionID = b.idLocked()
			if sessionID == "" {
				sessionID = "default"
			}
			harnessSession = b.harness.Session(sessionID)
		}
		b.mu.Unlock()
		if !b.isActiveTurn(turnID) {
			return abort(context.Canceled)
		}
	}

	if ionStore != nil {
		if err := ionStore.UpdateSession(turnCtx, storage.SessionInfo{
			ID:          sessionID,
			Model:       modelName,
			LastPreview: input,
			Title:       input,
		}); err != nil {
			return abort(fmt.Errorf("update session metadata: %w", err))
		}
	}
	if !b.isActiveTurn(turnID) {
		return abort(context.Canceled)
	}

	return submittedTurn{
		id:        turnID,
		ctx:       turnCtx,
		cancel:    cancel,
		submitter: cantoSessionTurnSubmitter{session: harnessSession},
	}, nil
}

func (b *Backend) acceptSubmittedTurn(
	submitted submittedTurn,
	input string,
) (cantoTurnHandle, error) {
	turn, err := submitted.submitter.submit(submitted.ctx, input)
	if err != nil {
		submitted.cancel()
		b.finishTurnIfActive(submitted.id)
		return nil, err
	}
	return turn, nil
}

type turnSubmitter interface {
	submit(context.Context, string) (cantoTurnHandle, error)
}

type cantoSessionTurnSubmitter struct {
	session *cantofw.Session
}

func (s cantoSessionTurnSubmitter) submit(
	ctx context.Context,
	message string,
) (cantoTurnHandle, error) {
	return s.session.Submit(ctx, message)
}

type cantoTurnHandle interface {
	Events() <-chan cantofw.RunEvent
	Cancel(context.Context) error
	Result() (agent.StepResult, error)
}

func (b *Backend) runTurn(
	ctx context.Context,
	turnID uint64,
	cancel context.CancelFunc,
	turn cantoTurnHandle,
) {
	defer b.clearActiveTurn(turnID)
	defer cancel()

	b.bindTurnCancel(turnID, func() {
		cancel()
		_ = turn.Cancel(context.Background())
	})

	terminal := false
	for event := range turn.Events() {
		if b.translateRunEvent(ctx, event, turnID) {
			terminal = true
		}
	}
	if _, err := turn.Result(); err != nil && !terminal && b.acceptsTurnEvent(turnID) {
		b.finishTurnWithError(turnID, err)
	}
}

func (b *Backend) finishTurnWithError(turnID uint64, err error) {
	if err == nil {
		return
	}
	base := ionsession.BaseNow()
	if isCancellationTerminal(err.Error()) {
		b.emitTurnFinished(turnID, base)
		return
	}
	b.emitTurnError(turnID, base, err)
}

func tokenUsageFromCantoUsage(usage llm.Usage) (ionsession.TokenUsage, bool) {
	total := usage.TotalTokens
	if total == 0 {
		total = usage.InputTokens + usage.OutputTokens
	}
	if usage.InputTokens == 0 && usage.OutputTokens == 0 && total == 0 && usage.Cost == 0 {
		return ionsession.TokenUsage{}, false
	}
	return ionsession.TokenUsage{
		Input:  usage.InputTokens,
		Output: usage.OutputTokens,
		Total:  total,
		Cost:   usage.Cost,
	}, true
}

func tokenUsageFromRunUsage(usage *cantofw.RunUsage) (ionsession.TokenUsage, bool) {
	if usage == nil {
		return ionsession.TokenUsage{}, false
	}
	return tokenUsageFromCantoUsage(usage.Delta)
}

func (b *Backend) emitRunUsage(base ionsession.Base, usage *cantofw.RunUsage) {
	msg, ok := tokenUsageFromRunUsage(usage)
	if !ok {
		return
	}
	msg.Base = base
	b.events <- msg
}

func (b *Backend) translateRunEvent(
	ctx context.Context,
	event cantofw.RunEvent,
	turnID uint64,
) bool {
	if !b.acceptsTurnEvent(turnID) {
		return false
	}

	switch event.Type {
	case cantofw.RunEventChunk:
		if b.isCancelingTurn(turnID) {
			return false
		}
		chunk := event.Chunk
		base := ionsession.BaseNow()
		if chunk.Reasoning != "" {
			b.events <- ionsession.ThinkingDelta{Base: base, Delta: chunk.Reasoning}
		}
		if chunk.Content != "" {
			b.events <- ionsession.AgentDelta{Base: base, Delta: chunk.Content}
		}
		b.emitRunUsage(base, event.Usage)
	case cantofw.RunEventSession:
		return b.translateRunSessionEvent(ctx, event, turnID)
	case cantofw.RunEventRetry:
		return b.translateRunSessionEvent(ctx, event, turnID)
	case cantofw.RunEventError:
		if event.Err == nil {
			return false
		}
		if b.isCancelingTurn(turnID) || isCancellationTerminal(event.Err.Error()) {
			return b.emitTurnFinished(turnID, ionsession.BaseNow())
		}
		return b.emitTurnError(turnID, ionsession.BaseNow(), event.Err)
	case cantofw.RunEventResult:
		base := ionsession.BaseNow()
		b.emitRunUsage(base, event.Usage)
		return b.emitTurnFinished(turnID, base)
	}
	return false
}

func (b *Backend) emitTurnError(turnID uint64, base ionsession.Base, err error) bool {
	if b.isCancelingTurn(turnID) {
		return b.emitTurnFinished(turnID, base)
	}
	if !b.claimTerminalTurn(turnID) {
		return false
	}
	b.events <- ionsession.Error{Base: base, Err: err}
	b.events <- ionsession.TurnFinished{Base: base}
	return true
}

func (b *Backend) emitTurnFinished(turnID uint64, base ionsession.Base) bool {
	if !b.claimTerminalTurn(turnID) {
		return false
	}
	b.events <- ionsession.TurnFinished{Base: base}
	return true
}

func (b *Backend) claimTerminalTurn(turnID uint64) bool {
	if turnID == 0 {
		return true
	}
	return b.finishTurnIfActive(turnID)
}

func (b *Backend) finishTurnIfActive(turnID uint64) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.turn.finish(turnID)
}

func (b *Backend) isActiveTurn(turnID uint64) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.turn.activeFor(turnID)
}

func (b *Backend) clearActiveTurn(turnID uint64) {
	b.mu.Lock()
	b.turn.finish(turnID)
	b.mu.Unlock()
}

func (b *Backend) bindTurnCancel(turnID uint64, cancel context.CancelFunc) {
	b.mu.Lock()
	if b.turn.activeFor(turnID) {
		b.turn.cancel = cancel
	}
	b.mu.Unlock()
}

func (b *Backend) acceptsTurnEvent(turnID uint64) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.turn.accepts(turnID)
}

func (b *Backend) isCancelingTurn(turnID uint64) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.turn.isCanceling(turnID)
}

func (s *Session) SteerTurn(
	ctx context.Context,
	text string,
) (ionsession.SteeringResult, error) {
	return s.backend.steerTurn(ctx, text)
}

func (b *Backend) steerTurn(
	ctx context.Context,
	text string,
) (ionsession.SteeringResult, error) {
	b.mu.Lock()
	active := b.turn.active
	activeTool := b.turn.hasActiveTool()
	sessionID := b.idLocked()
	steering := b.steering
	b.mu.Unlock()

	if !active || !activeTool || steering == nil {
		return ionsession.SteeringResult{
			Outcome: ionsession.SteeringQueued,
			Notice:  "No active provider boundary is available.",
		}, nil
	}
	if sessionID == "" {
		sessionID = "default"
	}
	return steering.Submit(ctx, sessionID, text)
}

func (s *Session) CancelTurn(ctx context.Context) error {
	return s.backend.cancelTurn(ctx)
}

func (b *Backend) cancelTurn(context.Context) error {
	b.mu.Lock()
	cancel, _ := b.turn.requestCancel()
	b.mu.Unlock()

	if cancel != nil {
		cancel()
	}
	return nil
}

func (b *Backend) markToolActive(turnID uint64, id string) {
	if id == "" {
		return
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	b.turn.markToolActive(turnID, id)
}

func (b *Backend) markToolComplete(turnID uint64, id string) (bool, bool) {
	if id == "" {
		return false, false
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.turn.markToolComplete(turnID, id)
}

func (b *Backend) setActiveTools(turnID uint64, tools []cantofw.RunToolLifecycle) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.turn.setActiveTools(turnID, tools)
}
