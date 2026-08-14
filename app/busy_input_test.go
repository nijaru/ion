package app

import (
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
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

	updated, cmd := model.queueBusyInput("continue with tests", nil)
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

func TestBusyInputFollowUpCarriesImageOnlyAttachment(t *testing.T) {
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner
	model.InFlight.Thinking = true
	model.Input.Images = []session.ImageContent{{Data: []byte("image"), MimeType: "image/png"}}

	updated, cmd := model.queueFollowUp()
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("image-only follow-up should return an asynchronous command")
	}
	if _, followUpCmd := model.Update(cmd()); followUpCmd != nil {
		t.Fatal("successful image-only follow-up should not emit an error command")
	}
	if len(runner.followUps) != 1 || runner.followUps[0] != "" {
		t.Fatalf("follow-ups = %#v, want one empty-text follow-up", runner.followUps)
	}
	if len(runner.followUpImages) != 1 || len(runner.followUpImages[0]) != 1 ||
		string(runner.followUpImages[0][0].Data) != "image" {
		t.Fatalf("follow-up images = %#v, want one image attachment", runner.followUpImages)
	}
}

func TestBusyInputQueueUsesBoundedRuntimeNextTurn(t *testing.T) {
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner
	model.Model.Config.BusyInput = "queue"
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("run after this turn")

	updated, cmd := model.submitBusyInput("run after this turn", nil)
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("queue mode should return an asynchronous command")
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q while queue request is pending, want empty", got)
	}

	next, resultCmd := model.Update(cmd())
	model = testModel(t, next)
	if resultCmd != nil {
		t.Fatal("successful next-turn queue should not emit an error command")
	}
	if len(runner.nextTurns) != 1 || runner.nextTurns[0] != "run after this turn" {
		t.Fatalf("next turns = %#v, want one runtime-owned next turn", runner.nextTurns)
	}
}

func TestBusyInputQueueFailureRestoresDraft(t *testing.T) {
	runner := &stubRunner{nextTurnErr: errors.New("queue full")}
	model := readyModel(t)
	model.Model.Runner = runner
	model.Model.Config.BusyInput = "queue"
	model.InFlight.Thinking = true

	updated, cmd := model.submitBusyInput("retry after capacity", nil)
	model = testModel(t, updated)
	next, resultCmd := model.Update(cmd())
	model = testModel(t, next)
	if got := model.Input.Composer.Value(); got != "retry after capacity" {
		t.Fatalf("composer after failed next turn = %q, want restored draft", got)
	}
	messages := runCommandTree(t, resultCmd)
	if len(messages) != 1 ||
		!strings.Contains(localErrorFromMsg(t, messages[0]).Error(), "next-turn input: queue full") {
		t.Fatalf("queue error messages = %#v, want visible queue failure", messages)
	}
}

func TestAltUpRecallsQueuedInputIntoComposer(t *testing.T) {
	runner := &stubRunner{
		followUps:      []string{"first follow up", "second follow up"},
		followUpImages: [][]session.ImageContent{nil, {{Data: []byte("img"), MimeType: "image/png"}}},
	}
	model := readyModel(t)
	model.Model.Runner = runner
	model.InFlight.Thinking = true

	// Press Alt+Up
	next, cmd := model.Update(tea.KeyPressMsg{Code: 'k', Mod: tea.ModAlt, Text: "alt+up"})
	model = testModel(t, next)
	if cmd != nil {
		runCommandTree(t, cmd)
	}

	if got := model.Input.Composer.Value(); got != "second follow up" {
		t.Fatalf("composer value after alt+up = %q, want %q", got, "second follow up")
	}
	if len(model.Input.Images) != 1 || string(model.Input.Images[0].Data) != "img" {
		t.Fatalf("composer images = %#v, want 1 image attachment", model.Input.Images)
	}

	// Press Alt+Up again to recall the first follow-up
	next, cmd = model.Update(tea.KeyPressMsg{Code: 'k', Mod: tea.ModAlt, Text: "alt+up"})
	model = testModel(t, next)
	if cmd != nil {
		runCommandTree(t, cmd)
	}
	if got := model.Input.Composer.Value(); got != "first follow up" {
		t.Fatalf("composer value after 2nd alt+up = %q, want %q", got, "first follow up")
	}
}

func TestAltUpFallsBackToHistoryWhenQueueEmpty(t *testing.T) {
	runner := &stubRunner{}
	model := readyModel(t)
	model.Model.Runner = runner
	model.Input.History = []string{"historical prompt"}

	// Press Alt+Up when queue is empty -> should retrieve historical prompt
	next, cmd := model.Update(tea.KeyPressMsg{Code: 'k', Mod: tea.ModAlt, Text: "alt+up"})
	model = testModel(t, next)
	if cmd != nil {
		runCommandTree(t, cmd)
	}
	if got := model.Input.Composer.Value(); got != "historical prompt" {
		t.Fatalf("composer value after alt+up with empty queue = %q, want %q", got, "historical prompt")
	}
}
