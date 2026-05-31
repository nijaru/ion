package session

import "testing"

func TestDecideTurnFinishMode(t *testing.T) {
	tests := []struct {
		name  string
		input TurnFinishModeInput
		want  TurnFinishModeDecision
	}{
		{
			name:  "existing error wins",
			input: TurnFinishModeInput{HadError: true},
			want: TurnFinishModeDecision{
				Action:      TurnFinishPreserveError,
				ClearQueued: true,
			},
		},
		{
			name: "budget cancellation wins before assistant check",
			input: TurnFinishModeInput{
				BudgetStopReason: "turn cost limit reached",
			},
			want: TurnFinishModeDecision{
				Action:      TurnFinishBudgetCancel,
				ClearQueued: true,
			},
		},
		{
			name: "canceling user cancellation preserves queued turns",
			input: TurnFinishModeInput{
				Canceled:  true,
				Canceling: true,
			},
			want: TurnFinishModeDecision{Action: TurnFinishUserCancel},
		},
		{
			name: "settled user cancellation clears queued turns",
			input: TurnFinishModeInput{
				Canceled: true,
			},
			want: TurnFinishModeDecision{
				Action:      TurnFinishUserCancel,
				ClearQueued: true,
			},
		},
		{
			name: "missing assistant becomes terminal error",
			input: TurnFinishModeInput{
				AssistantCompleted: false,
			},
			want: TurnFinishModeDecision{
				Action:       TurnFinishMissingAgent,
				ClearQueued:  true,
				DisplayError: "turn finished without assistant response",
				EntryContent: "Error: turn finished without assistant response",
			},
		},
		{
			name: "assistant completion completes turn",
			input: TurnFinishModeInput{
				AssistantCompleted: true,
			},
			want: TurnFinishModeDecision{Action: TurnFinishComplete},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideTurnFinishMode(tt.input); got != tt.want {
				t.Fatalf("DecideTurnFinishMode() = %#v, want %#v", got, tt.want)
			}
		})
	}
}
