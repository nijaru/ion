package agent

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestEventDeliveryChannelAndListenersShareExactOrder(t *testing.T) {
	h := NewHarness(HarnessConfig{
		Session: newTestSession(t),
		Events:  make(chan session.Event, 1),
	})

	want := []session.Event{
		session.AgentStart{},
		session.TurnStart{},
		session.SavePoint{HadPendingMutations: true},
		session.Settled{NextTurnCount: 2},
		session.AgentEnd{},
	}
	var listenerTypes []string
	unsubscribe := h.Subscribe(func(event session.Event) {
		listenerTypes = append(listenerTypes, fmt.Sprintf("%T", event))
	})
	defer unsubscribe()

	for _, event := range want {
		h.emit(event)
	}

	got := make([]session.Event, 0, len(want))
	for range want {
		select {
		case event := <-h.Events():
			got = append(got, event)
		case <-time.After(time.Second):
			t.Fatal("timed out draining event channel")
		}
	}
	if len(listenerTypes) != len(want) {
		t.Fatalf("listener events = %d, want %d", len(listenerTypes), len(want))
	}
	for i, event := range got {
		gotType := fmt.Sprintf("%T", event)
		wantType := fmt.Sprintf("%T", want[i])
		if gotType != wantType || listenerTypes[i] != wantType {
			t.Fatalf("event %d: channel=%s listener=%s want=%s", i, gotType, listenerTypes[i], wantType)
		}
	}
	if err := h.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

func TestEventDeliveryListenerCanEnqueueFollowUp(t *testing.T) {
	h := NewHarness(HarnessConfig{
		Session: newTestSession(t),
		Events:  make(chan session.Event, 1),
	})
	defer h.Close()

	h.Subscribe(func(event session.Event) {
		if _, ok := event.(session.AgentStart); ok {
			h.NextTurn("follow-up")
		}
	})

	h.emit(session.AgentStart{})
	for i := range 2 {
		select {
		case event := <-h.Events():
			if i == 0 {
				if _, ok := event.(session.AgentStart); !ok {
					t.Fatalf("event 0 = %T, want AgentStart", event)
				}
			} else if _, ok := event.(*session.QueueUpdate); !ok {
				t.Fatalf("event 1 = %T, want *session.QueueUpdate", event)
			}
		case <-time.After(time.Second):
			t.Fatalf("timed out waiting for event %d", i)
		}
	}
}

func TestSubscribeUnsubscribeSurvivesListenerGrowth(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: newTestSession(t)})
	defer h.Close()

	called := 0
	unsubscribe := h.Subscribe(func(session.Event) { called++ })
	for range 32 {
		h.Subscribe(func(session.Event) {})
	}
	unsubscribe()
	h.emit(session.AgentStart{})
	if called != 0 {
		t.Fatalf("unsubscribed listener called %d times", called)
	}
}

func TestEventDeliveryConcurrentCloseIsRaceFree(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: newTestSession(t)})
	var done sync.WaitGroup
	for range 64 {
		done.Add(1)
		go func() {
			defer done.Done()
			h.emit(session.AgentStart{})
		}()
	}

	closeDone := make(chan error, 1)
	go func() { closeDone <- h.Close() }()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close blocked while emitters were racing with shutdown")
	}
	done.Wait()
}

func TestEventDeliveryNoReaderCloseIsCancellable(t *testing.T) {
	h := NewHarness(HarnessConfig{
		Session: newTestSession(t),
		Events:  make(chan session.Event, 1),
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "done", StopReason: "stop"}}}, nil
		},
		Model: llm.Model{ID: "test"},
	})

	promptDone := make(chan error, 1)
	go func() {
		_, err := h.Prompt(context.Background(), "no reader")
		promptDone <- err
	}()
	select {
	case err := <-promptDone:
		if err != nil {
			t.Fatalf("Prompt: %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Prompt blocked without an event reader")
	}

	closeDone := make(chan error, 1)
	go func() { closeDone <- h.Close() }()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close blocked on an unread event channel")
	}
}

func newTestSession(t *testing.T) session.Session {
	t.Helper()
	return session.NewSession(newTestStore(t), 64)
}
