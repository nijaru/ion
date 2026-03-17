package acp

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/session"
)

func TestACPBackend(t *testing.T) {
	// Simple mock agent that echoes input as status and then finishes
	mockAgentScript := `
while read line; do
  # Very simplistic JSON parsing for the submit type
  if [[ "$line" == *"submit"* ]]; then
    echo '{"type": "turn_started", "data": {}}'
    echo '{"type": "status_changed", "data": {"status": "Mock processing..."}}'
    echo '{"type": "assistant_delta", "data": {"delta": "Echo: "}}'
    echo '{"type": "assistant_delta", "data": {"delta": "result"}}'
    echo '{"type": "turn_finished", "data": {}}'
  fi
done
`
	t.Setenv("ION_ACP_COMMAND", mockAgentScript)

	b := New()
	sess := b.Session()

	if err := sess.Open(context.Background()); err != nil {
		t.Fatalf("failed to open session: %v", err)
	}
	defer sess.Close()

	err := sess.SubmitTurn(context.Background(), "hello")
	if err != nil {
		t.Fatalf("failed to submit turn: %v", err)
	}

	// Collect events
	var events []session.Event
	timeout := time.After(2 * time.Second)
	done := false

	for !done {
		select {
		case ev, ok := <-sess.Events():
			if !ok {
				done = true
				break
			}
			t.Logf("Received event: %#v", ev)
			events = append(events, ev)
			if _, ok := ev.(session.TurnFinished); ok {
				done = true
			}
		case <-timeout:
			t.Fatalf("timed out waiting for events")
		}
	}

	// Verify events
	foundStatus := false
	var fullText strings.Builder
	for _, ev := range events {
		switch e := ev.(type) {
		case session.StatusChanged:
			if e.Status == "Mock processing..." {
				foundStatus = true
			}
		case session.AssistantDelta:
			fullText.WriteString(e.Delta)
		}
	}

	if !foundStatus {
		t.Error("StatusChanged event not found")
	}
	if fullText.String() != "Echo: result" {
		t.Errorf("expected 'Echo: result', got %q", fullText.String())
	}
}
