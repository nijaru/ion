package session

import (
	"testing"
)

func TestACRFenceValidateReuseCompletedOutput(t *testing.T) {
	sess := New("sess")
	if err := sess.Append(t.Context(), NewToolCompletedEvent(sess.ID(), ToolCompletedData{
		Tool:           "read_file",
		ID:             "call-1",
		IdempotencyKey: "key-1",
		Output:         "cached output",
	})); err != nil {
		t.Fatalf("append tool completed: %v", err)
	}

	decision, err := (ACRFence{}).Validate(sess, "key-1")
	if err != nil {
		t.Fatalf("Validate: %v", err)
	}
	if decision.Action != ReplayReuse || decision.Output != "cached output" {
		t.Fatalf("unexpected decision: %+v", decision)
	}
}

func TestACRFenceValidateRejectsAmbiguousStartedOnlyExecution(t *testing.T) {
	sess := New("sess")
	if err := sess.Append(t.Context(), NewToolStartedEvent(sess.ID(), ToolStartedData{
		Tool:           "read_file",
		Arguments:      `{"path":"main.go"}`,
		ID:             "call-1",
		IdempotencyKey: "key-1",
	})); err != nil {
		t.Fatalf("append tool started: %v", err)
	}

	if _, err := (ACRFence{}).Validate(sess, "key-1"); err == nil {
		t.Fatal("expected ambiguous replay error, got nil")
	}
}
