package session

import "testing"

func TestDecideTurnSettlement(t *testing.T) {
	tests := []struct {
		name  string
		input TurnSettlementInput
		want  TurnSettlementDecision
	}{
		{
			name:  "awaits with no queued local turns",
			input: TurnSettlementInput{},
			want:  TurnSettlementDecision{Action: TurnSettlementAwait},
		},
		{
			name: "awaits when backend owns the queue",
			input: TurnSettlementInput{
				BackendOwnedQueued: true,
				LocalQueuedTurns:   []string{"next"},
			},
			want: TurnSettlementDecision{Action: TurnSettlementAwait},
		},
		{
			name: "submits the oldest local queued turn",
			input: TurnSettlementInput{
				LocalQueuedTurns: []string{"first", "second"},
			},
			want: TurnSettlementDecision{
				Action: TurnSettlementSubmitLocal,
				Text:   "first",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideTurnSettlement(tt.input); got != tt.want {
				t.Fatalf("DecideTurnSettlement() = %#v, want %#v", got, tt.want)
			}
		})
	}
}

func TestDecideTurnFinishedDispatch(t *testing.T) {
	tests := []struct {
		name  string
		input TurnFinishedDispatchInput
		want  TurnFinishedDispatchDecision
	}{
		{
			name:  "awaits and refreshes git stats with no queued local turns",
			input: TurnFinishedDispatchInput{},
			want: TurnFinishedDispatchDecision{
				Action:        TurnFinishedDispatchAwait,
				ReloadGitDiff: true,
				AwaitNext:     true,
			},
		},
		{
			name: "awaits backend-owned queue",
			input: TurnFinishedDispatchInput{
				BackendOwnedQueued: true,
				LocalQueuedTurns:   []string{"backend follow-up"},
			},
			want: TurnFinishedDispatchDecision{
				Action:        TurnFinishedDispatchAwait,
				ReloadGitDiff: true,
				AwaitNext:     true,
			},
		},
		{
			name: "submits local follow-up and rearms session events",
			input: TurnFinishedDispatchInput{
				LocalQueuedTurns: []string{"first", "second"},
			},
			want: TurnFinishedDispatchDecision{
				Action:             TurnFinishedDispatchSubmitLocal,
				Text:               "first",
				RearmSessionEvents: true,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideTurnFinishedDispatch(tt.input); got != tt.want {
				t.Fatalf("DecideTurnFinishedDispatch() = %#v, want %#v", got, tt.want)
			}
		})
	}
}
