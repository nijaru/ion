package canto

import (
	"context"
	"errors"
	"testing"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestTranslateHarnessEventEmitsSavePoint(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	if !b.acceptTurn(turnID, "turn-1") {
		t.Fatal("accept turn failed")
	}

	translateRunHarnessEventForTest(t, b, turnID, cantofw.HarnessEvent{
		TurnID: "turn-1",
		Payload: cantofw.SavePointPayload{
			HadPendingMutations: true,
		},
	})

	savePoint, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnSavePoint)
	if !ok {
		t.Fatalf("event = %T, want TurnSavePoint", savePoint)
	}
	if !savePoint.HadPendingMutations {
		t.Fatal("save point did not preserve pending mutation flag")
	}
}

func TestTranslateHarnessEventSettledDoesNotFinishActiveTurn(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	if !b.acceptTurn(turnID, "turn-1") {
		t.Fatal("accept turn failed")
	}

	translateRunHarnessEventForTest(t, b, turnID, cantofw.HarnessEvent{
		TurnID:  "turn-1",
		Payload: cantofw.SettledPayload{},
	})

	assertNoBackendEvent(t, b)
	if !b.isActiveTurn(turnID) {
		t.Fatal("settled event finished turn before final run payload")
	}

	terminal := b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID:  "turn-1",
		Payload: cantofw.RunResultPayload{},
	}, turnID)
	if !terminal {
		t.Fatal("final run payload was not recognized")
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("final run payload after settlement did not emit TurnFinished")
	}
	if b.isActiveTurn(turnID) {
		t.Fatal("turn remained active after final run payload")
	}
}

func TestRunLifecycleTurnDoesNotFinishBeforeFinalResult(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	if !b.acceptTurn(turnID, "turn-1") {
		t.Fatal("accept turn failed")
	}

	terminal := b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID: "turn-1",
		Payload: cantofw.RunSessionPayload{Event: csession.NewTurnCompletedEvent(
			"session-id",
			csession.TurnCompletedData{},
		)},
		Lifecycle: &cantofw.RunLifecycle{
			Type:     cantofw.RunLifecycleTurn,
			Status:   cantofw.RunLifecycleCompleted,
			Terminal: true,
		},
	}, turnID)
	if terminal {
		t.Fatal("durable turn lifecycle claimed final run")
	}
	assertNoBackendEvent(t, b)
	if !b.isActiveTurn(turnID) {
		t.Fatal("turn finished before final run payload")
	}

	translateRunHarnessEventForTest(t, b, turnID, cantofw.HarnessEvent{
		TurnID:  "turn-1",
		Payload: cantofw.SettledPayload{},
	})
	assertNoBackendEvent(t, b)
	if !b.isActiveTurn(turnID) {
		t.Fatal("settled event finished turn before final run payload")
	}

	terminal = b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID:  "turn-1",
		Payload: cantofw.RunResultPayload{},
	}, turnID)
	if !terminal {
		t.Fatal("final run result was not recognized")
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("final run result did not emit TurnFinished")
	}
}

func TestRunLifecycleTurnErrorWaitsForFinalRunError(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	if !b.acceptTurn(turnID, "turn-1") {
		t.Fatal("accept turn failed")
	}

	terminal := b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID: "turn-1",
		Payload: cantofw.RunSessionPayload{Event: csession.NewTurnCompletedEvent(
			"session-id",
			csession.TurnCompletedData{Error: "provider failed"},
		)},
		Lifecycle: &cantofw.RunLifecycle{
			Type:     cantofw.RunLifecycleTurn,
			Status:   cantofw.RunLifecycleFailed,
			Error:    "provider failed",
			Terminal: true,
		},
	}, turnID)
	if terminal {
		t.Fatal("durable turn error lifecycle claimed final run")
	}
	assertNoBackendEvent(t, b)
	if !b.isActiveTurn(turnID) {
		t.Fatal("turn finished before final run error")
	}

	translateRunHarnessEventForTest(t, b, turnID, cantofw.HarnessEvent{
		TurnID:  "turn-1",
		Payload: cantofw.SettledPayload{},
	})
	assertNoBackendEvent(t, b)

	err := errors.New("provider failed")
	terminal = b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID:  "turn-1",
		Payload: cantofw.RunErrorPayload{Err: err},
	}, turnID)
	if !terminal {
		t.Fatal("terminal run error was not recognized")
	}
	errEvent, ok := receiveEvent(t, b.Session().Events()).(ionsession.Error)
	if !ok {
		t.Fatalf("event = %T, want Error", errEvent)
	}
	if !errors.Is(errEvent.Err, err) {
		t.Fatalf("error = %v, want %v", errEvent.Err, err)
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("terminal run error did not emit TurnFinished")
	}
}

func TestSettledBeforeTerminalErrorStillEmitsErrorThenFinished(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	if !b.acceptTurn(turnID, "turn-1") {
		t.Fatal("accept turn failed")
	}

	translateRunHarnessEventForTest(t, b, turnID, cantofw.HarnessEvent{
		TurnID:  "turn-1",
		Payload: cantofw.SettledPayload{},
	})
	assertNoBackendEvent(t, b)

	err := context.DeadlineExceeded
	terminal := b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID:  "turn-1",
		Payload: cantofw.RunErrorPayload{Err: err},
	}, turnID)
	if !terminal {
		t.Fatal("terminal run error was not recognized")
	}
	errEvent, ok := receiveEvent(t, b.Session().Events()).(ionsession.Error)
	if !ok {
		t.Fatalf("first event = %T, want Error", errEvent)
	}
	if errEvent.Err != err {
		t.Fatalf("error = %v, want %v", errEvent.Err, err)
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("terminal error after settlement did not emit TurnFinished")
	}
}

func TestSettledBeforeCanceledRunErrorStillFinishesQuietly(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	if !b.acceptTurn(turnID, "turn-1") {
		t.Fatal("accept turn failed")
	}
	if _, active := b.turn.requestCancel(); !active {
		t.Fatal("cancel request reported inactive turn")
	}

	translateRunHarnessEventForTest(t, b, turnID, cantofw.HarnessEvent{
		TurnID:  "turn-1",
		Payload: cantofw.SettledPayload{},
	})
	assertNoBackendEvent(t, b)

	terminal := b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID:  "turn-1",
		Payload: cantofw.RunErrorPayload{Err: context.Canceled},
	}, turnID)
	if !terminal {
		t.Fatal("terminal cancel error was not recognized")
	}
	if _, ok := receiveEvent(t, b.Session().Events()).(ionsession.TurnFinished); !ok {
		t.Fatal("canceled run error after settlement did not emit TurnFinished")
	}
	assertNoBackendEvent(t, b)
}

func TestTranslateHarnessEventIgnoresActiveTurnRuntimeEvents(t *testing.T) {
	b := New()
	turnID := b.turn.start(func() {})
	if !b.acceptTurn(turnID, "turn-1") {
		t.Fatal("accept turn failed")
	}

	b.translateHarnessEvent(cantofw.HarnessEvent{
		TurnID:  "turn-1",
		Payload: cantofw.SettledPayload{},
	})

	assertNoBackendEvent(t, b)
	if !b.isActiveTurn(turnID) {
		t.Fatal("active turn runtime event finished the turn")
	}
}

func translateRunHarnessEventForTest(
	t *testing.T,
	b *Backend,
	turnID uint64,
	event cantofw.HarnessEvent,
) bool {
	t.Helper()
	return b.translateRunEvent(t.Context(), cantofw.RunEvent{
		TurnID:  event.TurnID,
		Payload: cantofw.RunHarnessPayload{Event: event},
	}, turnID)
}

func TestSubmitTurnEmitsSavePointBeforeTurnFinished(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := llm.NewFauxProvider("openai", llm.FauxStep{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	savePointIdx, finishedIdx := -1, -1
	for i := 0; finishedIdx < 0; i++ {
		switch receiveEvent(t, b.Session().Events()).(type) {
		case ionsession.TurnSavePoint:
			savePointIdx = i
		case ionsession.TurnFinished:
			finishedIdx = i
		}
	}
	if savePointIdx < 0 {
		t.Fatal("turn finished before a Canto save point was observed")
	}
	if savePointIdx > finishedIdx {
		t.Fatalf("save point index %d after finished index %d", savePointIdx, finishedIdx)
	}
}
