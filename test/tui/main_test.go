package main

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestSmokeBackendEmitsEvents(t *testing.T) {
	ctx := context.Background()
	backend := newSmokeBackend("complete")

	events := []session.Event{
		userEvent("hello"),
		session.TurnStart{Timestamp: time.Now()},
		agentEndEvent("done"),
		session.TurnEnd{Base: session.BaseNow()},
	}

	go func() {
		for _, event := range events {
			if !backend.emit(ctx, event) {
				return
			}
		}
	}()

	for i := 0; i < len(events); i++ {
		select {
		case <-ctx.Done():
			t.Fatal("context done before receiving all events")
		case e := <-backend.Events():
			if e == nil {
				t.Fatalf("event %d is nil", i)
			}
		}
	}
}

func TestSmokeBackendCancelMode(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	backend := newSmokeBackend("cancel")

	go backend.runScript(ctx, "test input")

	// Should get UserMessage, TurnStart, StatusChange
	for _, expected := range []string{"UserMessage", "TurnStart", "StatusChange"} {
		select {
		case <-ctx.Done():
			t.Fatalf("context done before receiving %s", expected)
		case e := <-backend.Events():
			if e == nil {
				t.Fatalf("received nil event, expected %s", expected)
			}
		}
	}

	// Cancel to unblock the script.
	cancel()
}

func TestSmokeBackendErrorMode(t *testing.T) {
	ctx := context.Background()
	backend := newSmokeBackend("error")

	go backend.runScript(ctx, "test input")

	// Should get UserMessage, TurnStart, StatusChange, TurnEnd
	for i := 0; i < 4; i++ {
		select {
		case <-ctx.Done():
			t.Fatalf("context done before receiving event %d", i)
		case e := <-backend.Events():
			if e == nil {
				t.Fatalf("received nil event at index %d", i)
			}
		}
	}
}
