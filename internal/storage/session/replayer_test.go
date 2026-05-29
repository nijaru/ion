package session

import (
	"slices"
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

func TestReplayerReplayReconstructsEventsAndReducerState(t *testing.T) {
	replayer := NewReplayer(WithReplayReducer(func(state map[string]any, e Event) map[string]any {
		count, _ := state["events"].(int)
		state["events"] = count + 1
		if e.Type == MessageAdded {
			state["last_type"] = "message"
		}
		return state
	}))

	events := []Event{
		NewMessage("sess", llm.Message{Role: llm.RoleUser, Content: "hello"}),
		NewWaitStartedEvent("sess", WaitData{Reason: "approval"}),
	}

	sess, err := replayer.Replay("", slices.Values(events))
	if err != nil {
		t.Fatalf("Replay: %v", err)
	}
	if sess.ID() != "sess" {
		t.Fatalf("session id = %q, want sess", sess.ID())
	}
	if got := len(sess.Events()); got != 2 {
		t.Fatalf("expected 2 events, got %d", got)
	}
	state := sess.State()
	if got := state["events"]; got != 2 {
		t.Fatalf("state events = %#v, want 2", got)
	}
	if got := state["last_type"]; got != "message" {
		t.Fatalf("state last_type = %#v, want %q", got, "message")
	}
}

func TestReplayerApplyRejectsMixedSessionIDs(t *testing.T) {
	replayer := NewReplayer()
	sess := replayer.NewSession("parent")

	if err := replayer.Apply(sess, NewMessage("parent", llm.Message{Role: llm.RoleUser, Content: "ok"})); err != nil {
		t.Fatalf("Apply(parent): %v", err)
	}
	if err := replayer.Apply(sess, NewMessage("other", llm.Message{Role: llm.RoleUser, Content: "bad"})); err == nil {
		t.Fatal("expected mismatch error, got nil")
	}
}

func TestReplayerSupportsEmptyReplay(t *testing.T) {
	replayer := NewReplayer()

	sess, err := replayer.Replay("empty", slices.Values([]Event(nil)))
	if err != nil {
		t.Fatalf("Replay(empty): %v", err)
	}
	if sess.ID() != "empty" {
		t.Fatalf("session id = %q, want empty", sess.ID())
	}
	if len(sess.Events()) != 0 {
		t.Fatalf("expected empty event log, got %d events", len(sess.Events()))
	}
}

func TestReplayerPreservesCompactedHistoryProjections(t *testing.T) {
	replayer := NewReplayer()
	events := []Event{
		NewMessage("sess", llm.Message{Role: llm.RoleUser, Content: "old user"}),
		NewMessage("sess", llm.Message{Role: llm.RoleAssistant, Content: "old assistant"}),
		NewMessage("sess", llm.Message{Role: llm.RoleUser, Content: "recent user"}),
	}
	events = append(events, NewCompactionEvent("sess", CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: events[2].ID.String(),
		Entries: []HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>summary</conversation_summary>",
				},
			},
			{
				EventID: events[2].ID.String(),
				Message: llm.Message{Role: llm.RoleUser, Content: "recent user"},
			},
		},
	}))

	sess, err := replayer.Replay("sess", slices.Values(events))
	if err != nil {
		t.Fatalf("Replay(compacted): %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 2 {
		t.Fatalf("expected 2 effective messages, got %d", len(messages))
	}
	if messages[0].Role != llm.RoleUser || messages[1].Content != "recent user" {
		t.Fatalf("unexpected effective messages: %#v", messages)
	}
}
