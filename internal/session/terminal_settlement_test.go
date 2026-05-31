package session

import (
	"errors"
	"testing"
)

func TestDecideStreamClosure(t *testing.T) {
	if got := DecideStreamClosure(StreamClosureInput{}); got.Terminal {
		t.Fatalf("DecideStreamClosure() = %#v, want non-terminal idle stream close", got)
	}

	got := DecideStreamClosure(StreamClosureInput{Thinking: true})
	if !got.Terminal ||
		got.DisplayError != "session event stream closed" ||
		got.EntryContent != "Error: session event stream closed" {
		t.Fatalf("DecideStreamClosure() = %#v", got)
	}
}

func TestDecideErrorSettlement(t *testing.T) {
	tests := []struct {
		name        string
		input       ErrorSettlementInput
		wantDisplay string
		wantEntry   string
		wantPersist bool
		wantAwait   bool
		wantStop    *RoutingStop
	}{
		{
			name: "plain error",
			input: ErrorSettlementInput{
				Err:           errors.New("backend failed"),
				AwaitTerminal: true,
			},
			wantDisplay: "backend failed",
			wantEntry:   "Error: backend failed",
			wantPersist: true,
			wantAwait:   true,
		},
		{
			name:        "local submit error does not persist or await",
			input:       ErrorSettlementInput{Err: errors.New("submit failed")},
			wantDisplay: "submit failed",
			wantEntry:   "Error: submit failed",
		},
		{
			name: "provider limit produces routing stop",
			input: ErrorSettlementInput{
				Err:           errors.New("status code: 429 Too Many Requests"),
				AwaitTerminal: true,
			},
			wantDisplay: "API rate limit: status code: 429 Too Many Requests",
			wantEntry:   "Error: API rate limit: status code: 429 Too Many Requests",
			wantPersist: true,
			wantAwait:   true,
			wantStop: &RoutingStop{
				Reason:     "rate_limit",
				StopReason: "status code: 429 Too Many Requests",
			},
		},
		{
			name:        "friendly empty response",
			input:       ErrorSettlementInput{Err: errors.New("assistant response has no content")},
			wantDisplay: "Provider returned an empty response. Try again or switch models.",
			wantEntry:   "Error: Provider returned an empty response. Try again or switch models.",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := DecideErrorSettlement(tt.input)
			if got.DisplayError != tt.wantDisplay ||
				got.EntryContent != tt.wantEntry ||
				got.PersistSystem != tt.wantPersist ||
				got.AwaitNext != tt.wantAwait {
				t.Fatalf("DecideErrorSettlement() = %#v", got)
			}
			if !routingStopEqual(got.RoutingStop, tt.wantStop) {
				t.Fatalf("RoutingStop = %#v, want %#v", got.RoutingStop, tt.wantStop)
			}
		})
	}
}

func routingStopEqual(a, b *RoutingStop) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}
