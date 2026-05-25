package canto

import (
	"context"

	cantofw "github.com/nijaru/canto"
	ionsession "github.com/nijaru/ion/internal/session"
)

func (b *Backend) startRuntimeEvents(sessionID string, harness *cantofw.Harness) {
	if harness == nil {
		return
	}
	if sessionID == "" {
		sessionID = "default"
	}
	ctx, cancel := context.WithCancel(context.Background())

	b.mu.Lock()
	if b.runtimeEventsSessionID == sessionID && b.runtimeEventsCancel != nil {
		b.mu.Unlock()
		cancel()
		return
	}
	previous := b.runtimeEventsCancel
	b.runtimeEventsCancel = cancel
	b.runtimeEventsSessionID = sessionID
	b.mu.Unlock()

	if previous != nil {
		previous()
	}
	session := harness.Session(sessionID)
	b.wg.Go(func() {
		b.forwardRuntimeEvents(ctx, session)
	})
}

func (b *Backend) forwardRuntimeEvents(ctx context.Context, session *cantofw.Session) {
	events, err := session.RuntimeEvents(ctx)
	if err != nil {
		if ctx.Err() == nil {
			b.events <- ionsession.Error{Base: ionsession.BaseNow(), Err: err}
		}
		return
	}
	for event := range events {
		b.translateHarnessEvent(event)
	}
}

func (b *Backend) translateHarnessEvent(event cantofw.HarnessEvent) {
	if event.TurnID != "" {
		return
	}
	b.translateHarnessEventPayload(event, ionsession.BaseNow())
}

func (b *Backend) translateTurnHarnessEvent(event cantofw.HarnessEvent, base ionsession.Base) bool {
	return b.translateHarnessEventPayload(event, base)
}

func (b *Backend) translateHarnessEventPayload(
	event cantofw.HarnessEvent,
	base ionsession.Base,
) bool {
	switch payload := event.Payload.(type) {
	case cantofw.QueueUpdatedPayload:
		b.events <- ionsession.QueuedInputUpdated{
			Base:     base,
			Snapshot: queuedInputSnapshotFromCanto(payload.Queue),
		}
	case cantofw.SavePointPayload:
		b.events <- ionsession.TurnSavePoint{
			Base:                base,
			HadPendingMutations: payload.HadPendingMutations,
		}
	case cantofw.SettledPayload:
		return b.emitTurnSettled(event.TurnID, base)
	}
	return false
}

func (b *Backend) emitTurnSettled(cantoTurnID string, base ionsession.Base) bool {
	if !b.finishTurnByCantoID(cantoTurnID) {
		return false
	}
	b.events <- ionsession.TurnFinished{Base: base}
	return true
}
