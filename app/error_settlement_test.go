package app

import (
	"errors"
	"testing"

	"github.com/nijaru/ion/internal/agent"
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
