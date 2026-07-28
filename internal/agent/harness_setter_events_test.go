package agent

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessSettersEmitLifecycleUpdates(t *testing.T) {
	store := newTestStore(t)
	h := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
		Model:   llm.Model{Provider: "old-provider", ID: "old-model"},
	})
	defer func() { _ = h.Close() }()

	events := make(chan session.Event, 3)
	unsubscribe := watchEvents(t, h, func(event session.Event) { events <- event })
	defer unsubscribe()

	h.SetModel(llm.Model{Provider: "new-provider", ID: "new-model"})
	if err := h.SetThinking(context.Background(), session.ThinkingHigh); err != nil {
		t.Fatal(err)
	}
	h.SetTools([]Tool{{Name: "read"}}, []string{"read"})

	selectEvent := func() session.Event {
		t.Helper()
		select {
		case event := <-events:
			return event
		case <-time.After(time.Second):
			t.Fatal("timed out waiting for setter event")
			return nil
		}
	}

	selectSetterEvent := func() session.Event {
		t.Helper()
		for {
			event := selectEvent()
			if _, ok := event.(session.RuntimeReady); ok {
				// Idle setter persistence uses the same busy barrier as turn
				// finalization. RuntimeReady is lifecycle noise for this
				// setter-order assertion, but it must remain observable to
				// resyncing clients.
				continue
			}
			return event
		}
	}

	modelEvent := selectSetterEvent()
	model, ok := modelEvent.(session.ModelUpdate)
	if !ok {
		t.Fatalf("first event = %T, want ModelUpdate", modelEvent)
	}
	if model.Model != "new-model" || model.Previous != "old-model" || model.Source != session.UpdateSourceSet {
		t.Fatalf("model update = %#v", model)
	}
	thinkingEvent := selectSetterEvent()
	thinking, ok := thinkingEvent.(session.ThinkingUpdate)
	if !ok {
		t.Fatalf("second event = %T, want ThinkingUpdate", thinkingEvent)
	}
	if thinking.Level != session.ThinkingHigh || thinking.Previous != "" {
		t.Fatalf("thinking update = %#v", thinking)
	}
	toolsEvent := selectSetterEvent()
	tools, ok := toolsEvent.(session.ToolsUpdate)
	if !ok {
		t.Fatalf("third event = %T, want ToolsUpdate", toolsEvent)
	}
	if len(tools.Active) != 1 || tools.Active[0] != "read" || len(tools.Previous) != 0 {
		t.Fatalf("tools update = %#v", tools)
	}
}

func TestIdleModelSetterPublishesRuntimeReady(t *testing.T) {
	store := newTestStore(t)
	h := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
		Model:   llm.Model{Provider: "old-provider", ID: "old-model"},
	})
	defer func() { _ = h.Close() }()

	sub, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer sub.Close()

	if err := h.SetModel(llm.Model{Provider: "new-provider", ID: "new-model"}); err != nil {
		t.Fatalf("SetModel: %v", err)
	}

	var events []session.Event
	for len(events) < 2 {
		select {
		case envelope := <-sub.Events:
			events = append(events, envelope.Event)
		case <-time.After(time.Second):
			t.Fatalf("timed out waiting for setter lifecycle events: %v", eventNames(events))
		}
	}
	if _, ok := events[0].(session.ModelUpdate); !ok {
		t.Fatalf("first setter event = %T, want ModelUpdate", events[0])
	}
	if _, ok := events[1].(session.RuntimeReady); !ok {
		t.Fatalf("second setter event = %T, want RuntimeReady", events[1])
	}
}
