package session

import (
	"testing"
	"time"
)

func TestDecideCancelStart(t *testing.T) {
	now := time.Date(2026, 5, 31, 3, 0, 0, 0, time.UTC)

	got := DecideCancelStart(CancelStartInput{
		Reason: "Canceled by user",
		Now:    now,
	})
	want := CancelStartDecision{
		EntryContent:   "Canceled by user",
		DrainStartedAt: now,
		ClearQueued:    true,
		Thinking:       true,
		Canceling:      true,
	}
	if got != want {
		t.Fatalf("DecideCancelStart() = %#v, want %#v", got, want)
	}
}

func TestDecideCancelFinish(t *testing.T) {
	tests := []struct {
		name  string
		input CancelFinishInput
		want  CancelFinishDecision
	}{
		{
			name:  "preserves queued follow-up while cancel is settling",
			input: CancelFinishInput{Canceling: true},
			want:  CancelFinishDecision{PreserveQueued: true},
		},
		{
			name:  "clears queued follow-up for ordinary cancelled mode",
			input: CancelFinishInput{},
			want:  CancelFinishDecision{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideCancelFinish(tt.input); got != tt.want {
				t.Fatalf("DecideCancelFinish() = %#v, want %#v", got, tt.want)
			}
		})
	}
}
