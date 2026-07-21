package app

import (
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

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

func TestBusyInputControlErrorIsVisibleAndRestoresDraft(t *testing.T) {
	runner := &stubRunner{steerErr: errors.New("no active turn")}
	model := readyModel(t)
	model.Model.Runner = runner
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("redirect the work")

	updated, cmd := model.submitBusyInput("redirect the work", nil)
	if cmd == nil {
		t.Fatal("steer should return an asynchronous command")
	}
	model = testModel(t, updated)
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q while steer is pending, want empty", got)
	}

	result := cmd()
	var next tea.Model
	next, cmd = model.Update(result)
	model = testModel(t, next)
	if got := model.Input.Composer.Value(); got != "redirect the work" {
		t.Fatalf("composer after failed steer = %q, want restored draft", got)
	}
	if len(runner.steers) != 1 || runner.steers[0] != "redirect the work" {
		t.Fatalf("steers = %#v, want one submitted steer", runner.steers)
	}

	messages := runCommandTree(t, cmd)
	if len(messages) != 1 {
		t.Fatalf("error command messages = %d, want one", len(messages))
	}
	err := localErrorFromMsg(t, messages[0])
	if !strings.Contains(err.Error(), "steer input: no active turn") {
		t.Fatalf("error = %q, want visible steer failure", err)
	}
}

func TestBusyInputFollowUpSuccessUsesRuntimeCommand(t *testing.T) {
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("continue with tests")

	updated, cmd := model.queueBusyInput("continue with tests")
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("follow-up should return an asynchronous command")
	}
	var next tea.Model
	next, followUpCmd := model.Update(cmd())
	model = testModel(t, next)
	if followUpCmd != nil {
		t.Fatal("successful follow-up should not emit an error command")
	}
	if len(runner.followUps) != 1 || runner.followUps[0] != "continue with tests" {
		t.Fatalf("follow-ups = %#v, want one submitted follow-up", runner.followUps)
	}
}
