package canto

import (
	"context"
	"fmt"

	cantofw "github.com/nijaru/canto"
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

	b.wg.Add(1)
	go func() {
		defer b.wg.Done()
		defer b.finishActiveTurn(turnID)
		defer cancel()

		shouldCompact, err := b.shouldProactivelyCompact(turnCtx)
		if err != nil {
			b.events <- ionsession.Error{Err: err}
			b.finishTurn(turnID)
			b.events <- ionsession.TurnFinished{}
			return
		}
		if shouldCompact {
			b.events <- ionsession.StatusChanged{Status: "Compacting context..."}
			if compacted, cerr := b.Compact(turnCtx); cerr != nil {
				b.events <- ionsession.Error{Err: cerr}
				b.finishTurn(turnID)
				b.events <- ionsession.TurnFinished{}
				return
			} else if compacted {
				b.events <- ionsession.StatusChanged{Status: "Ready"}
			}
		}

		runEvents, err := harnessSession.PromptStream(turnCtx, input)
		if err != nil {
			b.events <- ionsession.Error{Err: err}
			b.finishTurn(turnID)
			b.events <- ionsession.TurnFinished{}
			return
		}
		for event := range runEvents {
			b.translateRunEvent(turnCtx, event, turnID)
		}
	}()

	return nil
}

func (b *Backend) translateRunEvent(ctx context.Context, event cantofw.RunEvent, turnID uint64) {
	switch event.Type {
	case cantofw.RunEventChunk:
		chunk := event.Chunk
		if chunk.Reasoning != "" {
			b.events <- ionsession.ThinkingDelta{Delta: chunk.Reasoning}
		}
		if chunk.Content != "" {
			b.events <- ionsession.AgentDelta{Delta: chunk.Content}
		}
		if chunk.Usage != nil {
			b.events <- ionsession.TokenUsage{
				Input:  chunk.Usage.InputTokens,
				Output: chunk.Usage.OutputTokens,
				Cost:   chunk.Usage.Cost,
			}
		}
	case cantofw.RunEventSession:
		b.translateEvent(ctx, event.Event, turnID)
	case cantofw.RunEventError:
		if event.Err == nil || isCancellationTerminal(event.Err.Error()) {
			return
		}
		if b.turnActiveFor(turnID) {
			b.events <- ionsession.Error{Err: event.Err}
			b.finishTurn(turnID)
			b.events <- ionsession.TurnFinished{}
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
		b.events <- ionsession.TurnFinished{}
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
