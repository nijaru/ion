package session

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

type failingWriter struct {
	err error
}

func (w *failingWriter) Save(_ context.Context, _ Event) error {
	return w.err
}

func TestSessionAppend_DoesNotMutateStateWhenWriterFails(t *testing.T) {
	sess := New("append-fail").WithWriter(&failingWriter{err: errors.New("boom")})
	sess.WithReducer(func(state map[string]any, e Event) map[string]any {
		count, _ := state["count"].(int)
		state["count"] = count + 1
		return state
	})

	subCtx, cancel := context.WithCancel(t.Context())
	defer cancel()
	sub := sess.Watch(subCtx)
	defer sub.Close()

	err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	}))
	if err == nil {
		t.Fatal("expected append to fail")
	}
	if len(sess.Events()) != 0 {
		t.Fatalf("expected no in-memory events after failed append, got %d", len(sess.Events()))
	}
	if len(sess.State()) != 0 {
		t.Fatalf("expected reducer state to remain empty, got %#v", sess.State())
	}

	select {
	case e := <-sub.Events():
		t.Fatalf("unexpected subscriber event after failed append: %#v", e)
	default:
	}
}

func TestSessionAppendRejectsEmptyAssistantMessage(t *testing.T) {
	sess := New("append-empty-assistant")

	err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:      llm.RoleAssistant,
		Content:   " \n\t ",
		Reasoning: " ",
	}))
	if err == nil || !strings.Contains(err.Error(), "assistant message has no content") {
		t.Fatalf("append error = %v, want empty assistant rejection", err)
	}
	if len(sess.Events()) != 0 {
		t.Fatalf("events = %#v, want none", sess.Events())
	}
}

func TestSessionAppendRejectsUnknownMessageRole(t *testing.T) {
	sess := New("append-unknown-role")

	err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Content: "missing role",
	}))
	if !errors.Is(err, errInvalidMessageRole) {
		t.Fatalf("append error = %v, want %v", err, errInvalidMessageRole)
	}
	if len(sess.Events()) != 0 {
		t.Fatalf("events = %#v, want none", sess.Events())
	}
}

func TestSessionAppendPreservesAssistantPayloadKinds(t *testing.T) {
	sess := New("append-assistant-payloads")
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"

	for _, msg := range []llm.Message{
		{Role: llm.RoleAssistant, Content: "content"},
		{Role: llm.RoleAssistant, Reasoning: "reasoning"},
		{Role: llm.RoleAssistant, ThinkingBlocks: []llm.ThinkingBlock{{Type: "thinking", Thinking: "step"}}},
		{Role: llm.RoleAssistant, Calls: []llm.Call{call}},
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append payload-bearing assistant: %v", err)
		}
	}

	if got := len(sess.Messages()); got != 4 {
		t.Fatalf("messages = %d, want 4", got)
	}
}

func TestSessionAppendRejectsUnmatchedToolMessage(t *testing.T) {
	sess := New("append-unmatched-tool")

	err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "call-1",
		Name:    "read",
		Content: "result",
	}))
	if !errors.Is(err, errUnmatchedToolMessage) {
		t.Fatalf("append error = %v, want %v", err, errUnmatchedToolMessage)
	}
	if len(sess.Events()) != 0 {
		t.Fatalf("events = %#v, want none", sess.Events())
	}
}

func TestSessionAppendPreservesMatchedToolMessage(t *testing.T) {
	sess := New("append-matched-tool")
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"

	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append assistant tool call: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "call-1",
		Name:    "read",
		Content: "result",
	})); err != nil {
		t.Fatalf("append matched tool message: %v", err)
	}
	if got := len(sess.Messages()); got != 2 {
		t.Fatalf("messages = %d, want 2", got)
	}
}

func TestSessionAppendRejectsLateToolMessageAfterTurnBoundary(t *testing.T) {
	sess := New("append-late-tool")
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"

	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append assistant tool call: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "next turn"); err != nil {
		t.Fatalf("append user: %v", err)
	}
	err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "call-1",
		Name:    "read",
		Content: "late result",
	}))
	if !errors.Is(err, errUnmatchedToolMessage) {
		t.Fatalf("append error = %v, want %v", err, errUnmatchedToolMessage)
	}
}

func TestLastAssistantMessageSkipsLegacyEmptyAssistant(t *testing.T) {
	replayer := NewReplayer()
	sess := replayer.NewSession("legacy-last-assistant")
	if err := replayer.Apply(sess, NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: "valid answer",
	})); err != nil {
		t.Fatalf("replay valid assistant: %v", err)
	}
	if err := replayer.Apply(sess, NewMessage(sess.ID(), llm.Message{
		Role: llm.RoleAssistant,
	})); err != nil {
		t.Fatalf("replay legacy empty assistant: %v", err)
	}

	msg, ok := sess.LastAssistantMessage()
	if !ok {
		t.Fatal("expected valid assistant")
	}
	if msg.Content != "valid answer" {
		t.Fatalf("last assistant content = %q, want valid answer", msg.Content)
	}
}

func TestSessionBranchRequiresSessionBranchWriter(t *testing.T) {
	sess := New("parent").WithWriter(&failingWriter{})

	if _, err := sess.Branch(t.Context(), "child", ForkOptions{}); err == nil {
		t.Fatal("expected branch without session-branch writer to fail")
	}
}
