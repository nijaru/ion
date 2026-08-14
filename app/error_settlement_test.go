package app

import (
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/agent"
	"github.com/nijaru/ion/session"
)

func TestDecideErrorSettlementSurfacesErrorAndAwaitsTerminalEvent(t *testing.T) {
	decision := DecideErrorSettlement(ErrorSettlementInput{
		AwaitTerminal: true,
		Err:           errors.New("provider unavailable"),
	})
	if !decision.AwaitNext {
		t.Fatal("terminal error did not request the next lifecycle event")
	}
	if decision.DisplayError != "provider unavailable" {
		t.Fatalf("display error = %q, want provider unavailable", decision.DisplayError)
	}
	if decision.EntryContent != "Error: provider unavailable" {
		t.Fatalf("entry content = %q, want prefixed error", decision.EntryContent)
	}
}

func TestTurnErrorKeepsSubscriptionOpenUntilSettled(t *testing.T) {
	model := readyModel(t)
	model.Model.EventSubscription = &agent.EventSubscription{
		Events: make(chan agent.EventEnvelope),
	}
	model.InFlight.Thinking = true
	model.Progress.Mode = StateStreaming

	next, cmd := model.handleSessionEvent(session.TurnEnd{Error: errors.New("provider unavailable")})
	if cmd == nil {
		t.Fatal("turn error did not keep the event stream active")
	}
	if next.Progress.Mode != StateError || next.Progress.LastError != "provider unavailable" {
		t.Fatalf("error projection = %#v, want terminal error", next.Progress)
	}
	if !next.InFlight.Thinking {
		t.Fatal("turn error cleared the active projection before Settled")
	}
	if !next.Model.EventSubscriptionState.readerBusy {
		t.Fatal("turn error did not arm the next event reader")
	}

	settled, _ := next.handleSettled(session.Settled{})
	if settled.Progress.Mode != StateReady || settled.InFlight.Thinking {
		t.Fatalf("settled projection = %#v/%#v, want ready and idle", settled.Progress, settled.InFlight)
	}
}

func TestLocalErrorClearsPreviousTerminalErrorWhenIdle(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = StateError
	model.Progress.LastError = "previous failure"
	model.Progress.Status = "stale"

	next, cmd := model.handleLocalError(errors.New("new failure"))
	if cmd == nil {
		t.Fatal("local error did not produce a terminal notice")
	}
	if next.Progress.Mode != StateReady || next.Progress.LastError != "" || next.Progress.Status != "" {
		t.Fatalf("local error projection = %#v, want idle without stale error state", next.Progress)
	}
}

func TestTurnReducerRecordsFinishedSummary(t *testing.T) {
	model := readyModel(t)
	started := time.Unix(100, 0)
	model.Progress.TurnStartedAt = started
	model.Progress.CurrentTurnInput = 120
	model.Progress.CurrentTurnOutput = 45
	model.Progress.CurrentTurnCost = 0.0123

	model.turnReducer().RecordFinishedTurnSummary(started.Add(1500 * time.Millisecond))
	want := TurnSummary{
		Elapsed: 1500 * time.Millisecond,
		Input:   120,
		Output:  45,
		Cost:    0.0123,
	}
	if model.Progress.LastTurnSummary != want {
		t.Fatalf("last turn summary = %#v, want %#v", model.Progress.LastTurnSummary, want)
	}
}

func TestTerminalTurnProjectsCompletionAndFailureUntilSettled(t *testing.T) {
	t.Run("success", func(t *testing.T) {
		model := readyModel(t)
		model.InFlight.Thinking = true
		model.InFlight.AgentCommitted = true
		model.InFlight.Pending = messageEntry(&session.AssistantMessage{})
		model.Progress.TurnStartedAt = time.Now().Add(-time.Second)
		model.Progress.CurrentTurnInput = 10
		model.Progress.CurrentTurnOutput = 20

		next, cmd := model.handleTurnFinished(session.TurnEnd{})
		if cmd == nil || next.Progress.Mode != StateComplete {
			t.Fatalf("completion projection = %#v, want complete with an event reader", next.Progress)
		}
		if next.Progress.LastTurnSummary.Input != 10 || next.Progress.LastTurnSummary.Output != 20 {
			t.Fatalf("completion summary = %#v, want current turn usage", next.Progress.LastTurnSummary)
		}
		settled, _ := next.handleSettled(session.Settled{})
		if settled.Progress.Mode != StateReady {
			t.Fatalf("settled completion mode = %v, want ready", settled.Progress.Mode)
		}
	})

	t.Run("failure", func(t *testing.T) {
		model := readyModel(t)
		model.InFlight.Thinking = true
		model.InFlight.AgentCommitted = true
		model.InFlight.Pending = messageEntry(&session.AssistantMessage{
			StopReason: session.StopReasonError,
			Error:      "provider unavailable",
		})

		next, cmd := model.handleTurnFinished(session.TurnEnd{})
		if cmd == nil || next.Progress.Mode != StateError ||
			next.Progress.LastError != "provider unavailable" {
			t.Fatalf("failure projection = %#v, want provider error", next.Progress)
		}
		settled, _ := next.handleSettled(session.Settled{})
		if settled.Progress.Mode != StateReady {
			t.Fatalf("settled failure mode = %v, want ready", settled.Progress.Mode)
		}
	})
}

func messageEntry(message session.Message) *session.Entry {
	var entry session.Entry = &session.MessageEntry{Message: message}
	return &entry
}
