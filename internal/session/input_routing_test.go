package session

import (
	"errors"
	"slices"
	"testing"
)

func TestRouteBusyInput(t *testing.T) {
	tests := []struct {
		name  string
		input BusyInputRouting
		want  BusyInputRoute
	}{
		{
			name: "steer mode uses steering during active turn",
			input: BusyInputRouting{
				Mode:             "steer",
				Thinking:         true,
				SupportsSteering: true,
				SupportsFollowUp: true,
			},
			want: BusyInputRouteSteer,
		},
		{
			name: "falls back to follow up without steering support",
			input: BusyInputRouting{
				Mode:             "steer",
				Thinking:         true,
				SupportsFollowUp: true,
			},
			want: BusyInputRouteFollowUp,
		},
		{
			name: "queue mode uses follow up",
			input: BusyInputRouting{
				Mode:             "queue",
				Thinking:         true,
				SupportsSteering: true,
				SupportsFollowUp: true,
			},
			want: BusyInputRouteFollowUp,
		},
		{
			name: "compaction keeps input local",
			input: BusyInputRouting{
				Mode:             "steer",
				Thinking:         true,
				Compacting:       true,
				SupportsSteering: true,
				SupportsFollowUp: true,
			},
			want: BusyInputRouteLocalQueue,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := RouteBusyInput(tt.input); got != tt.want {
				t.Fatalf("RouteBusyInput() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestDecideSteeringResult(t *testing.T) {
	tests := []struct {
		name   string
		result SteeringResult
		err    error
		want   SteeringResultDecision
	}{
		{
			name:   "accepted",
			result: SteeringResult{Outcome: SteeringAccepted},
			want: SteeringResultDecision{
				Action:        BusyInputResultAccepted,
				NoticeContent: "Steering current turn",
			},
		},
		{
			name:   "queued falls back locally",
			result: SteeringResult{Outcome: SteeringQueued},
			want:   SteeringResultDecision{Action: BusyInputResultFallback},
		},
		{
			name: "error falls back locally",
			err:  errors.New("backend unavailable"),
			want: SteeringResultDecision{Action: BusyInputResultFallback},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideSteeringResult(tt.result, tt.err); got != tt.want {
				t.Fatalf("DecideSteeringResult() = %#v, want %#v", got, tt.want)
			}
		})
	}
}

func TestDecideFollowUpResult(t *testing.T) {
	tests := []struct {
		name  string
		input FollowUpResultInput
		want  FollowUpResultDecision
	}{
		{
			name: "accepted appends new projection",
			input: FollowUpResultInput{
				Text:               "next",
				PriorFollowUpCount: 1,
				CurrentFollowUp:    []string{"first"},
				Result:             QueuedInputResult{Outcome: QueuedInputAccepted},
			},
			want: FollowUpResultDecision{
				Action:        BusyInputResultAccepted,
				FollowUp:      []string{"first", "next"},
				NoticeContent: "Queued follow-up",
			},
		},
		{
			name: "accepted preserves backend snapshot without duplicate",
			input: FollowUpResultInput{
				Text:               "next",
				PriorFollowUpCount: 1,
				CurrentFollowUp:    []string{"first", "next"},
				Result:             QueuedInputResult{Outcome: QueuedInputAccepted},
			},
			want: FollowUpResultDecision{
				Action:        BusyInputResultAccepted,
				FollowUp:      []string{"first", "next"},
				NoticeContent: "Queued follow-up",
			},
		},
		{
			name: "queued falls back locally",
			input: FollowUpResultInput{
				Text:   "next",
				Result: QueuedInputResult{Outcome: QueuedInputQueued},
			},
			want: FollowUpResultDecision{Action: BusyInputResultFallback},
		},
		{
			name: "error falls back locally",
			input: FollowUpResultInput{
				Text: "next",
				Err:  errors.New("backend unavailable"),
			},
			want: FollowUpResultDecision{Action: BusyInputResultFallback},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := DecideFollowUpResult(tt.input)
			if got.Action != tt.want.Action ||
				got.NoticeContent != tt.want.NoticeContent ||
				!slices.Equal(got.FollowUp, tt.want.FollowUp) {
				t.Fatalf("DecideFollowUpResult() = %#v, want %#v", got, tt.want)
			}
		})
	}
}
