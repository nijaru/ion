package app

import (
	"testing"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestApplyAgentRuntimeSnapshotRehydratesCompleteProjection(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.InFlight.QueuedSteering = []string{"stale steer"}
	model.InFlight.QueuedTurns = []string{"stale follow-up"}

	snapshot := agent.RuntimeSnapshot{
		SessionID: "session-resumed",
		Phase:     agent.PhaseStreaming,
		Model: llm.Model{
			Provider: "anthropic",
			ID:       "claude-test",
		},
		Thinking:    session.ThinkingHigh,
		ActiveTools: []string{"read", "edit"},
		Queues: agent.QueueSnapshot{
			Steer: []session.Message{
				&session.UserMessage{Content: []session.Content{session.TextContent{Text: "steer me"}}},
			},
			FollowUp: []session.Message{
				&session.UserMessage{Content: []session.Content{session.TextContent{Text: "follow up"}}},
			},
			NextTurn: []session.Message{
				&session.UserMessage{Content: []session.Content{session.TextContent{Text: "next turn"}}},
			},
		},
	}

	model.applyAgentRuntimeSnapshot(snapshot)

	if model.Model.Runtime.SessionID != "session-resumed" ||
		model.Model.Runtime.Provider != "anthropic" ||
		model.Model.Runtime.Model != "claude-test" ||
		model.Model.Runtime.Reasoning != "high" {
		t.Fatalf("runtime projection = %#v", model.Model.Runtime)
	}
	if !model.InFlight.Thinking || model.Progress.Mode != StateStreaming ||
		model.Progress.Status != "Streaming..." {
		t.Fatalf("turn projection = %#v progress=%#v", model.InFlight, model.Progress)
	}
	if got, want := model.InFlight.QueuedSteering, []string{"steer me"}; !equalStrings(got, want) {
		t.Fatalf("steering queue = %#v, want %#v", got, want)
	}
	if got, want := model.InFlight.QueuedTurns, []string{"follow up", "next turn"}; !equalStrings(got, want) {
		t.Fatalf("turn queue = %#v, want %#v", got, want)
	}
	if !equalStrings(model.Model.ActiveTools, []string{"read", "edit"}) {
		t.Fatalf("active tools = %#v", model.Model.ActiveTools)
	}
}

func TestSessionEventCursorAdvancesAfterAcceptedSequence(t *testing.T) {
	model := readyModel(t)
	stream := agent.EventStreamID{1}
	model.Model.EventGeneration = 7
	model.Model.EventCursor = agent.EventCursor{Stream: stream, Next: 12}
	model.Model.EventSubscription = &agent.EventSubscription{}

	next, cmd, handled := model.dispatchTurnControllerMessage(sessionEventMsg{
		generation: 7,
		cursor:     agent.EventCursor{Stream: stream, Next: 12},
		event:      session.SavePoint{},
	})
	if !handled {
		t.Fatal("session event was not handled")
	}
	if got, want := next.Model.EventCursor.Next, uint64(13); got != want {
		t.Fatalf("event cursor next = %d, want %d", got, want)
	}
	if cmd == nil {
		t.Fatal("accepted event did not re-arm subscription")
	}
}

func TestAwaitSessionEventDeduplicatesPendingSubscription(t *testing.T) {
	model := readyModel(t)

	first := model.awaitSessionEvent()
	if first == nil {
		t.Fatal("first subscription request is nil")
	}
	second := model.awaitSessionEvent()
	if second != nil {
		t.Fatal("duplicate subscription request was started while the first was pending")
	}
	if state := model.Model.EventSubscriptionState; state == nil || !state.pending {
		t.Fatalf("subscription state = %#v, want pending", state)
	}
}

func TestAwaitSessionEventDeduplicatesActiveReader(t *testing.T) {
	model := readyModel(t)
	model.Model.EventSubscription = &agent.EventSubscription{
		Events: make(chan agent.EventEnvelope),
	}

	first := model.awaitSessionEvent()
	if first == nil {
		t.Fatal("first event reader is nil")
	}
	second := model.awaitSessionEvent()
	if second != nil {
		t.Fatal("duplicate event reader was started while the first was pending")
	}
	if state := model.Model.EventSubscriptionState; state == nil || !state.readerBusy {
		t.Fatalf("subscription state = %#v, want active reader", state)
	}
}

func TestStaleEventReaderCannotAdvanceRuntimeCursor(t *testing.T) {
	model := readyModel(t)
	stream := agent.EventStreamID{1}
	model.Model.EventGeneration = 3
	model.Model.EventCursor = agent.EventCursor{Stream: stream, Next: 7}
	model.Model.EventSubscription = &agent.EventSubscription{}
	state := model.Model.EventSubscriptionState
	state.generation = 3
	state.reader = 2
	state.readerBusy = true

	next, cmd, handled := model.dispatchTurnControllerMessage(sessionEventMsg{
		generation: 3,
		reader:     1,
		cursor:     agent.EventCursor{Stream: stream, Next: 7},
		event:      session.SavePoint{},
	})
	if !handled {
		t.Fatal("stale event reader was not handled")
	}
	if cmd != nil {
		t.Fatal("stale event reader started a replacement command")
	}
	if got := next.Model.EventCursor.Next; got != 7 {
		t.Fatalf("cursor next = %d, want unchanged 7", got)
	}
	if !state.readerBusy {
		t.Fatal("current event reader was marked idle by stale result")
	}
}

func equalStrings(got, want []string) bool {
	if len(got) != len(want) {
		return false
	}
	for i := range got {
		if got[i] != want[i] {
			return false
		}
	}
	return true
}
