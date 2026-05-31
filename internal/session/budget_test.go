package session

import "testing"

func TestBudgetStopReason(t *testing.T) {
	tests := []struct {
		name  string
		input BudgetStopInput
		want  string
	}{
		{
			name: "no limits",
		},
		{
			name: "below limits",
			input: BudgetStopInput{
				CurrentTurnCost: 0.5,
				TotalCost:       1.0,
				MaxTurnCost:     1.0,
				MaxSessionCost:  2.0,
			},
		},
		{
			name: "turn limit wins",
			input: BudgetStopInput{
				CurrentTurnCost: 1.5,
				TotalCost:       3.0,
				MaxTurnCost:     1.0,
				MaxSessionCost:  2.0,
			},
			want: "turn cost limit reached ($1.500000 / $1.000000)",
		},
		{
			name: "session limit",
			input: BudgetStopInput{
				CurrentTurnCost: 0.5,
				TotalCost:       2.5,
				MaxTurnCost:     1.0,
				MaxSessionCost:  2.0,
			},
			want: "session cost limit reached ($2.500000 / $2.000000)",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := BudgetStopReason(tt.input); got != tt.want {
				t.Fatalf("BudgetStopReason() = %q, want %q", got, tt.want)
			}
		})
	}
}
