package agent

import (
	"context"
	"sync"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// eventRecorder records agent events with their types for ordered assertions.
type eventRecorder struct {
	events []string
}

func (r *eventRecorder) record(ev session.AgentEvent) {
	r.events = append(r.events, eventTypeName(ev))
}

func eventTypeName(ev session.AgentEvent) string {
	switch ev.(type) {
	case session.AgentStart:
		return "agent_start"
	case session.AgentEnd:
		return "agent_end"
	case session.TurnStart:
		return "turn_start"
	case session.TurnEnd:
		return "turn_end"
	case session.UserMessage:
		return "user_message"
	case session.AgentMessage:
		return "agent_message"
	case session.MessageUpdate:
		return "message_update"
	case session.ToolCallStart:
		return "tool_call_start"
	case session.ToolCallEnd:
		return "tool_call_end"
	case session.MessageStart:
		return "message_start"
	case session.MessageEnd:
		return "message_end"
	default:
		return "unknown"
	}
}

// assertEventSequence checks that the recorded events match the expected sequence.
// It allows extra events between expected events (prefix matching).
func assertEventSequence(t *testing.T, got []string, want []string) {
	t.Helper()
	
	gi := 0
	for _, expected := range want {
		found := false
		for gi < len(got) {
			if got[gi] == expected {
				found = true
				gi++
				break
			}
			gi++
		}
		if !found {
			t.Errorf("expected event %q not found in sequence (got through index %d of %d): %v", expected, gi, len(got), got)
			return
		}
	}
}

// assertEventOrder checks that events appear in the specified order.
// It does not require them to be adjacent — other events may appear between them.
func assertEventOrder(t *testing.T, got []string, want []string) {
	t.Helper()
	
	gi := 0
	for _, expected := range want {
		found := false
		for gi < len(got) {
			if got[gi] == expected {
				found = true
				gi++
				break
			}
			gi++
		}
		if !found {
			t.Errorf("expected event %q not found after index %d in: %v", expected, gi, got)
			return
		}
	}
}

// TestLoopEventSequence tests the basic event sequence for a simple run.
// Expected: agent_start → turn_start → agent_delta → agent_message → turn_end → agent_end
func TestLoopEventSequence(t *testing.T) {
	rec := &eventRecorder{}
	
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{Content: "hello"}}}, nil
	}
	
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		OnEvent:  rec.record,
	})
	
	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hi"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	
	// Verify the event sequence (agent_end is emitted by wrapper after loop returns)
	assertEventOrder(t, rec.events, []string{
		"agent_start",
		"user_message",
		"turn_start",
		"message_start",
		"message_update",
		"message_end",
		"turn_end",
	})
}

// TestLoopEventSequenceWithTool tests the event sequence with tool calls.
// Expected: agent_start → turn_start → agent_delta → tool_call_start → tool_call_end → turn_start → agent_delta → turn_end → agent_end
func TestLoopEventSequenceWithTool(t *testing.T) {
	rec := &eventRecorder{}
	
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		if len(rec.events) < 10 {
			// First turn: return tool call
			return &mockStream{chunks: []*llm.Chunk{{
				Content: "using tool",
				Calls:  []llm.Call{testCall("call-1", "read", `{}`)},
			}}}, nil
		}
		// Second turn: return final response
		return &mockStream{chunks: []*llm.Chunk{{Content: "done"}}}, nil
	}
	
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, tc AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("file contents")}}, nil
		},
		OnEvent: rec.record,
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			// Stop after tool results are processed
			return len(ctx.ToolResults) > 0
		},
	})
	agent.SetTools([]AgentTool{{Name: "read"}})
	
	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read file"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	
	// Verify the event sequence (agent_end is emitted by wrapper after loop returns)
	assertEventOrder(t, rec.events, []string{
		"agent_start",
		"user_message",
		"turn_start",
		"message_start",
		"message_update",       // "using tool"
		"message_end",
		"tool_call_start",
		"tool_call_end",
		"message_start",
		"message_end",
		"turn_end",
	})
}

// TestLoopEventSequenceWithSteering tests the event sequence with steering messages.
func TestLoopEventSequenceWithSteering(t *testing.T) {
	rec := &eventRecorder{}
	
	callCount := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callCount++
		if callCount <= 2 {
			return &mockStream{chunks: []*llm.Chunk{{Content: "working"}}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{{Content: "done"}}}, nil
	}
	
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		OnEvent:  rec.record,
		GetSteeringMessages: func() []AgentMessage {
			if callCount == 1 {
				return []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "steer"}}}}
			}
			return nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return callCount >= 3
		},
	})
	
	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "start"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	
	// Should have multiple turns due to steering (agent_end is emitted by wrapper)
	// Note: turn_start is emitted before steering messages are injected
	assertEventOrder(t, rec.events, []string{
		"agent_start",
		"user_message",    // initial prompt
		"turn_start",
		"message_start",
		"message_update",     // first response
		"message_end",
		"turn_end",
		"turn_start",      // second turn starts
		"user_message",    // steering message injected
		"message_start",
		"message_update",     // second response
		"message_end",
		"turn_end",
	})
}

// TestLoopEventSequenceWithFollowUp tests the event sequence with follow-up messages.
func TestLoopEventSequenceWithFollowUp(t *testing.T) {
	rec := &eventRecorder{}
	
	callCount := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callCount++
		if callCount == 1 {
			return &mockStream{chunks: []*llm.Chunk{{Content: "first response"}}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{{Content: "follow-up response"}}}, nil
	}
	
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		OnEvent:  rec.record,
		GetFollowUpMessages: func() []AgentMessage {
			if callCount == 1 {
				return []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "follow-up"}}}}
			}
			return nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return callCount >= 2
		},
	})
	
	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "start"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	
	// Should have two turns: initial + follow-up (agent_end is emitted by wrapper)
	// Note: turn_start is emitted before follow-up messages are injected
	assertEventOrder(t, rec.events, []string{
		"agent_start",
		"user_message",    // initial prompt
		"turn_start",
		"message_start",
		"message_update",     // first response
		"message_end",
		"turn_end",
		"turn_start",      // second turn starts
		"user_message",    // follow-up message injected
		"message_start",
		"message_update",     // follow-up response
		"message_end",
		"turn_end",
	})
}

// TestLoopEventSequenceCancellation tests that cancellation emits proper events.
func TestLoopEventSequenceCancellation(t *testing.T) {
	rec := &eventRecorder{}
	
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{Content: "started"}}}, nil
	}
	
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		OnEvent:  rec.record,
	})
	
	ctx, cancel := context.WithCancel(context.Background())
	cancel() // Cancel immediately
	
	_, err := agent.Run(ctx, []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "go"}}}})
	if err == nil {
		t.Fatal("expected error from cancelled context")
	}
	
	// Should have agent_start and turn_end (with cancellation)
	assertEventOrder(t, rec.events, []string{
		"agent_start",
		"turn_end",
	})
}

// TestLoopEventSequenceWithThinking tests the event sequence with thinking/reasoning.
func TestLoopEventSequenceWithThinking(t *testing.T) {
	rec := &eventRecorder{}
	
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Reasoning: "let me think..."},
			{Content: "here's my answer"},
		}}, nil
	}
	
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		OnEvent:  rec.record,
	})
	
	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "think"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	
	// Should have thinking_delta before agent_delta (agent_end is emitted by wrapper)
	assertEventOrder(t, rec.events, []string{
		"agent_start",
		"turn_start",
		"message_start",
		"message_update",
		"message_update",
		"message_end",
		"turn_end",
	})
}

func TestMessageUpdateDeltaAndBlockType(t *testing.T) {
	// Use a custom recorder that stores actual events
	type recordedEvent struct {
		Type string
		Ev   session.AgentEvent
	}
	var events []recordedEvent
	var mu sync.Mutex
	record := func(ev session.AgentEvent) {
		mu.Lock()
		defer mu.Unlock()
		events = append(events, recordedEvent{Type: eventTypeName(ev), Ev: ev})
	}
	
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Reasoning: "let me think..."},
			{Content: "here's my answer"},
		}}, nil
	}
	
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		OnEvent:  record,
	})
	
	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "think"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	
	// Find MessageUpdate events
	var updates []session.MessageUpdate
	mu.Lock()
	for _, rec := range events {
		if u, ok := rec.Ev.(session.MessageUpdate); ok {
			updates = append(updates, u)
		}
	}
	mu.Unlock()
	if len(updates) != 2 {
		t.Fatalf("expected 2 MessageUpdate events, got %d", len(updates))
	}
	
	// First should be thinking
	if updates[0].BlockType != "thinking" {
		t.Fatalf("first update BlockType = %q, want thinking", updates[0].BlockType)
	}
	if updates[0].Delta != "let me think..." {
		t.Fatalf("first update Delta = %q, want 'let me think...'", updates[0].Delta)
	}
	if updates[0].Message.Reasoning != "let me think..." {
		t.Fatalf("first update Message.Reasoning = %q, want 'let me think...'", updates[0].Message.Reasoning)
	}
	
	// Second should be text
	if updates[1].BlockType != "text" {
		t.Fatalf("second update BlockType = %q, want text", updates[1].BlockType)
	}
	if updates[1].Delta != "here's my answer" {
		t.Fatalf("second update Delta = %q, want 'here's my answer'", updates[1].Delta)
	}
	if updates[1].Message.Message != "here's my answer" {
		t.Fatalf("second update Message.Message = %q, want 'here's my answer'", updates[1].Message.Message)
	}
}
