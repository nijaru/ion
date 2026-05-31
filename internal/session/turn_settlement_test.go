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
