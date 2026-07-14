package agent

import (
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessSettersEmitLifecycleUpdates(t *testing.T) {
	store := newTestStore(t)
	h := NewHarness(HarnessConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
		Model:   llm.Model{Provider: "old-provider", ID: "old-model"},
	})
	defer func() { _ = h.Close() }()

	events := make(chan session.Event, 3)
	unsubscribe := h.Subscribe(func(event session.Event) { events <- event })
	defer unsubscribe()

	h.SetModel(llm.Model{Provider: "new-provider", ID: "new-model"})
	h.SetThinking(session.ThinkingHigh)
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

	modelEvent := selectEvent()
	model, ok := modelEvent.(session.ModelUpdate)
	if !ok {
		t.Fatalf("first event = %T, want ModelUpdate", modelEvent)
	}
	if model.Model != "new-model" || model.Previous != "old-model" || model.Source != session.UpdateSourceSet {
		t.Fatalf("model update = %#v", model)
	}
	thinkingEvent := selectEvent()
	thinking, ok := thinkingEvent.(session.ThinkingUpdate)
	if !ok {
		t.Fatalf("second event = %T, want ThinkingUpdate", thinkingEvent)
	}
	if thinking.Level != session.ThinkingHigh || thinking.Previous != "" {
		t.Fatalf("thinking update = %#v", thinking)
	}
	toolsEvent := selectEvent()
	tools, ok := toolsEvent.(session.ToolsUpdate)
	if !ok {
		t.Fatalf("third event = %T, want ToolsUpdate", toolsEvent)
	}
	if len(tools.Active) != 1 || tools.Active[0] != "read" || len(tools.Previous) != 0 {
		t.Fatalf("tools update = %#v", tools)
	}

}
