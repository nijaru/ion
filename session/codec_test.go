package session

import (
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestEventJSONHelpersRoundTripMetadata(t *testing.T) {
	event := NewMessage("portable-session", llm.Message{
		Role:    llm.RoleAssistant,
		Content: "done",
	})
	event.Metadata = map[string]any{
		"kind": "message",
		"seq":  float64(2),
	}

	data, err := MarshalEventJSON(event)
	if err != nil {
		t.Fatalf("marshal event: %v", err)
	}
	got, err := UnmarshalEventJSON(data)
	if err != nil {
		t.Fatalf("unmarshal event: %v", err)
	}
	if got.ID != event.ID || got.SessionID != event.SessionID || got.Type != event.Type {
		t.Fatalf("event identity = %#v, want %#v", got, event)
	}
	if err := got.ensureMetadata(); err != nil {
		t.Fatalf("ensure metadata: %v", err)
	}
	if got.Metadata["kind"] != "message" || got.Metadata["seq"] != float64(2) {
		t.Fatalf("metadata = %#v, want kind and seq", got.Metadata)
	}
}
