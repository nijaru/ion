package canto

import (
	"context"
	"fmt"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (b *Backend) SubmitTurn(ctx context.Context, input string) error {
	b.mu.Lock()
	if b.harness == nil {
		b.mu.Unlock()
		return fmt.Errorf("backend not initialized")
	}
	if b.turnActive {
		b.mu.Unlock()
		return fmt.Errorf("turn already in progress")
	}
	if lazy, ok := b.sess.(interface {
		Ensure(context.Context) (storage.Session, error)
	}); ok {
		sess, err := lazy.Ensure(ctx)
		if err != nil {
			b.mu.Unlock()
			return fmt.Errorf("open session: %w", err)
		}
		b.sess = sess
	}

	sessionID := b.ID()
	if sessionID == "" {
		sessionID = "default"
	}
	if b.ionStore != nil {
		if err := b.ionStore.UpdateSession(ctx, storage.SessionInfo{
			ID:          sessionID,
			Model:       storageModelName(b.Provider(), b.Model()),
			LastPreview: input,
			Title:       input,
		}); err != nil {
			b.mu.Unlock()
			return fmt.Errorf("update session metadata: %w", err)
		}
	}

	turnCtx, cancel := context.WithCancel(ctx)
	b.turnSeq++
	turnID := b.turnSeq
	b.turnActive = true
	b.cancel = cancel
	b.clearActiveToolsLocked()
	harnessSession := b.harness.Session(sessionID)
	b.mu.Unlock()

	b.wg.Go(func() {
		defer b.finishActiveTurn(turnID)
		defer cancel()

		shouldCompact, err := b.shouldProactivelyCompact(turnCtx)
		if err != nil {
			b.finishTurnWithError(turnID, err)
			return
		}
		if shouldCompact {
			b.events <- ionsession.StatusChanged{Base: ionsession.BaseNow(), Status: "Compacting context..."}
			if compacted, cerr := b.Compact(turnCtx); cerr != nil {
				b.finishTurnWithError(turnID, cerr)
				return
			} else if compacted {
				b.events <- ionsession.StatusChanged{Base: ionsession.BaseNow(), Status: "Ready"}
			}
		}

		runEvents, err := harnessSession.PromptStream(turnCtx, input)
		if err != nil {
			b.finishTurnWithError(turnID, err)
			return
		}
		usage := &turnUsageTracker{}
		for event := range runEvents {
			b.translateRunEvent(turnCtx, event, turnID, usage)
		}
	})

	return nil
}

func (b *Backend) finishTurnWithError(turnID uint64, err error) {
	base := ionsession.BaseNow()
	b.events <- ionsession.Error{Base: base, Err: err}
	b.finishTurn(turnID)
	b.events <- ionsession.TurnFinished{Base: base}
}

type turnUsageTracker struct {
	seen   bool
	input  int
	output int
	cost   float64
}

func (t *turnUsageTracker) reset() {
	*t = turnUsageTracker{}
}

func (t *turnUsageTracker) delta(usage *llm.Usage) (ionsession.TokenUsage, bool) {
	if usage == nil {
		return ionsession.TokenUsage{}, false
	}
	input := usage.InputTokens
	output := usage.OutputTokens
	cost := usage.Cost
	if t.seen && (input < t.input || output < t.output || cost < t.cost) {
		t.reset()
	}

	deltaInput := input
	deltaOutput := output
	deltaCost := cost
	if t.seen {
		deltaInput -= t.input
		deltaOutput -= t.output
		deltaCost -= t.cost
	}

	t.seen = true
	t.input = input
	t.output = output
	t.cost = cost

	if deltaInput == 0 && deltaOutput == 0 && deltaCost == 0 {
		return ionsession.TokenUsage{}, false
	}
	return ionsession.TokenUsage{
		Input:  deltaInput,
		Output: deltaOutput,
		Total:  deltaInput + deltaOutput,
		Cost:   deltaCost,
	}, true
}

func (b *Backend) translateRunEvent(
	ctx context.Context,
	event cantofw.RunEvent,
	turnID uint64,
	usage *turnUsageTracker,
) {
	if !b.acceptsTurnEvent(turnID) {
		return
	}

	switch event.Type {
	case cantofw.RunEventChunk:
		chunk := event.Chunk
		base := ionsession.BaseNow()
		if chunk.Reasoning != "" {
			b.events <- ionsession.ThinkingDelta{Base: base, Delta: chunk.Reasoning}
		}
		if chunk.Content != "" {
			b.events <- ionsession.AgentDelta{Base: base, Delta: chunk.Content}
		}
		if usage != nil {
			msg, ok := usage.delta(chunk.Usage)
			if !ok {
				return
			}
			msg.Base = base
			b.events <- msg
		}
	case cantofw.RunEventSession:
		b.translateEvent(ctx, event.Event, turnID)
		if usage != nil && event.Event.Type == csession.ToolCompleted {
			usage.reset()
		}
	case cantofw.RunEventError:
		if event.Err == nil || isCancellationTerminal(event.Err.Error()) {
			return
		}
		if b.turnActiveFor(turnID) {
			base := ionsession.BaseNow()
			b.events <- ionsession.Error{Base: base, Err: event.Err}
			b.finishTurn(turnID)
			b.events <- ionsession.TurnFinished{Base: base}
		}
	case cantofw.RunEventResult:
	}
}

func (b *Backend) finishTurn(turnID uint64) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.turnSeq == turnID {
		b.turnActive = false
		b.cancel = nil
		b.clearActiveToolsLocked()
	}
}

func (b *Backend) finishActiveTurn(turnID uint64) {
	b.mu.Lock()
	if b.turnSeq != turnID || !b.turnActive {
		b.mu.Unlock()
		return
	}
	b.turnActive = false
	b.cancel = nil
	b.clearActiveToolsLocked()
	b.mu.Unlock()

	b.events <- ionsession.TurnFinished{}
}

func (b *Backend) turnActiveFor(turnID uint64) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.turnSeq == turnID && b.turnActive
}

func (b *Backend) acceptsTurnEvent(turnID uint64) bool {
	if turnID == 0 {
		return true
	}
	return b.turnActiveFor(turnID)
}

func (b *Backend) SteerTurn(
	ctx context.Context,
	text string,
) (ionsession.SteeringResult, error) {
	b.mu.Lock()
	active := b.turnActive
	activeTool := len(b.activeToolIDs) > 0
	sessionID := b.ID()
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

func (b *Backend) CancelTurn(ctx context.Context) error {
	b.mu.Lock()
	cancel := b.cancel
	active := b.turnActive
	b.cancel = nil
	b.turnActive = false
	b.clearActiveToolsLocked()
	b.mu.Unlock()

	if cancel != nil {
		cancel()
	}
	if active {
		b.events <- ionsession.TurnFinished{Base: ionsession.BaseNow()}
	}
	return nil
}

func (b *Backend) markToolActive(turnID uint64, id string) {
	if id == "" {
		return
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.turnSeq != turnID || !b.turnActive {
		return
	}
	if b.activeToolIDs == nil {
		b.activeToolIDs = make(map[string]struct{})
	}
	b.activeToolIDs[id] = struct{}{}
}

func (b *Backend) markToolComplete(turnID uint64, id string) {
	if id == "" {
		return
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.turnSeq != turnID {
		return
	}
	delete(b.activeToolIDs, id)
}

func (b *Backend) clearActiveToolsLocked() {
	for id := range b.activeToolIDs {
		delete(b.activeToolIDs, id)
	}
}
