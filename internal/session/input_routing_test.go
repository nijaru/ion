package session

import "testing"

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
