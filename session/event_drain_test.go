package session

import (
	"testing"
	"time"
)

func TestDecideEventDrain(t *testing.T) {
	drainStartedAt := time.Date(2026, 5, 31, 4, 0, 0, 0, time.UTC)
	beforeDrain := drainStartedAt.Add(-time.Millisecond)
	afterDrain := drainStartedAt.Add(time.Millisecond)

	tests := []struct {
		name  string
		input EventDrainInput
		want  EventDrainDecision
	}{
		{
			name: "inactive drain processes event",
			input: EventDrainInput{
				Event: UserMessage{Base: BaseAt(beforeDrain), Message: "stale"},
			},
			want: EventDrainDecision{Action: EventDrainProcess},
		},
		{
			name: "drains stale user message",
			input: EventDrainInput{
				Active:         true,
				DrainStartedAt: drainStartedAt,
				Event:          UserMessage{Base: BaseAt(beforeDrain), Message: "stale"},
			},
			want: EventDrainDecision{Action: EventDrainAwait},
		},
		{
			name: "fresh user message finishes drain and processes",
			input: EventDrainInput{
				Active:         true,
				DrainStartedAt: drainStartedAt,
				Event:          UserMessage{Base: BaseAt(afterDrain), Message: "fresh"},
			},
			want: EventDrainDecision{Action: EventDrainProcess, FinishDrain: true},
		},
		{
			name: "zero user timestamp finishes drain and processes",
			input: EventDrainInput{
				Active:         true,
				DrainStartedAt: drainStartedAt,
				Event:          UserMessage{Message: "fresh enough"},
			},
			want: EventDrainDecision{Action: EventDrainProcess, FinishDrain: true},
		},
		{
			name: "drains stale turn start",
			input: EventDrainInput{
				Active:         true,
				DrainStartedAt: drainStartedAt,
				Event:          TurnStart{Base: BaseAt(beforeDrain)},
			},
			want: EventDrainDecision{Action: EventDrainAwait},
		},
		{
			name: "fresh turn start finishes drain and processes",
			input: EventDrainInput{
				Active:         true,
				DrainStartedAt: drainStartedAt,
				Event:          TurnStart{Base: BaseAt(afterDrain)},
			},
			want: EventDrainDecision{Action: EventDrainProcess, FinishDrain: true},
		},
		{
			name: "turn finished finishes drain and processes",
			input: EventDrainInput{
				Active:         true,
				DrainStartedAt: drainStartedAt,
				Event:          TurnEnd{Base: BaseAt(beforeDrain)},
			},
			want: EventDrainDecision{Action: EventDrainProcess, FinishDrain: true},
		},
		{
			name: "other events drain while waiting for fresh turn boundary",
			input: EventDrainInput{
				Active:         true,
				DrainStartedAt: drainStartedAt,
				Event:          AgentDelta{Base: BaseAt(afterDrain), Delta: "ignored"},
			},
			want: EventDrainDecision{Action: EventDrainAwait},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideEventDrain(tt.input); got != tt.want {
				t.Fatalf("DecideEventDrain() = %#v, want %#v", got, tt.want)
			}
		})
	}
}
