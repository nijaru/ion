package app

import "testing"

func TestRouteBusyInputUsesConfiguredMode(t *testing.T) {
	cases := []struct {
		name string
		in   BusyInputRouting
		want string
	}{
		{
			name: "default steer",
			in:   BusyInputRouting{Thinking: true, SupportsSteering: true},
			want: BusyInputRouteSteer,
		},
		{
			name: "follow up",
			in:   BusyInputRouting{Mode: BusyInputRouteFollowUp, Thinking: true, SupportsFollowUp: true},
			want: BusyInputRouteFollowUp,
		},
		{
			name: "queue",
			in:   BusyInputRouting{Mode: "queue", Thinking: true, SupportsSteering: true, SupportsFollowUp: true},
			want: "",
		},
		{
			name: "idle",
			in:   BusyInputRouting{Mode: BusyInputRouteSteer, SupportsSteering: true},
			want: "",
		},
		{
			name: "compacting",
			in:   BusyInputRouting{Mode: BusyInputRouteSteer, Thinking: true, Compacting: true, SupportsSteering: true},
			want: "",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := RouteBusyInput(tc.in); got != tc.want {
				t.Fatalf("RouteBusyInput(%#v) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
}
