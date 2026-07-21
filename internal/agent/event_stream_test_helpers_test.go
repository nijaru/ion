package agent

import (
	"context"
	"testing"

	"github.com/nijaru/ion/session"
)

// watchEvents gives tests an independent subscription without reintroducing
// the runtime's removed callback/listener delivery path.
func watchEvents(t *testing.T, h *Controller, fn func(session.Event)) func() {
	t.Helper()
	sub, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatalf("subscribe events: %v", err)
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for envelope := range sub.Events {
			if envelope.Event != nil {
				fn(envelope.Event)
			}
		}
	}()
	return func() {
		sub.Close()
		<-done
	}
}

func newTestSession(t *testing.T) session.Session {
	t.Helper()
	return session.NewSession(newTestStore(t), 64)
}
