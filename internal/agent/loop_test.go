package agent

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// --- helpers ---

type mockStream struct {
	chunks []*llm.Chunk
	idx    int
}

func (s *mockStream) Next() (*llm.Chunk, bool) {
	if s.idx >= len(s.chunks) {
		return nil, false
	}
	c := s.chunks[s.idx]
	s.idx++
	return c, true
}
func (s *mockStream) Err() error  { return nil }
func (s *mockStream) Close() error { return nil }

// --- Loop contract tests ---

// INVARIANT: RunLoop emits AgentStart, then AgentEnd (lifecycle bookends).
func TestRunLoopLifecycleEvents(t *testing.T) {
	var events []session.Event
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "hello", StopReason: "stop"},
		}}, nil
	}

	RunLoop(context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{SystemPrompt: "test"},
		LoopConfig{
			Model:    llm.Model{ID: "test"},
			StreamFn: streamFn,
		},
		func(e session.Event) { events = append(events, e) },
		nil,
	)

	if len(events) < 2 {
		t.Fatalf("expected at least 2 events, got %d", len(events))
	}
	if _, ok := events[0].(session.AgentStart); !ok {
		t.Fatalf("first event should be AgentStart, got %T", events[0])
	}
	if _, ok := events[len(events)-1].(session.AgentEnd); !ok {
		t.Fatalf("last event should be AgentEnd, got %T", events[len(events)-1])
	}
}

// INVARIANT: RunLoop emits exactly one AgentEnd per run (no multi-AgentEnd bug).
func TestRunLoopSingleAgentEnd(t *testing.T) {
	var agentEndCount int
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "ok", StopReason: "stop"},
		}}, nil
	}

	RunLoop(context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}, StreamFn: streamFn},
		func(e session.Event) {
			if _, ok := e.(session.AgentEnd); ok {
				agentEndCount++
			}
		},
		nil,
	)

	if agentEndCount != 1 {
		t.Fatalf("expected exactly 1 AgentEnd, got %d", agentEndCount)
	}
}

// INVARIANT: RunLoop returns new messages (prompts + assistant response).
func TestRunLoopReturnsNewMessages(t *testing.T) {
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "response", StopReason: "stop"},
		}}, nil
	}

	msgs := RunLoop(context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}, StreamFn: streamFn},
		func(e session.Event) {},
		nil,
	)

	if len(msgs) < 2 {
		t.Fatalf("expected at least 2 messages (prompt + response), got %d", len(msgs))
	}
	// First should be the user prompt.
	if _, ok := msgs[0].(*session.UserMessage); !ok {
		t.Fatalf("first message should be UserMessage, got %T", msgs[0])
	}
	// Last should be the assistant response.
	if _, ok := msgs[len(msgs)-1].(*session.AssistantMessage); !ok {
		t.Fatalf("last message should be AssistantMessage, got %T", msgs[len(msgs)-1])
	}
}

// INVARIANT: failure is terminal — error causes AgentEnd, not a retry loop.
func TestRunLoopTerminalFailure(t *testing.T) {
	var agentEndCount int
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return nil, context.DeadlineExceeded
	}

	msgs := RunLoop(context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}, StreamFn: streamFn},
		func(e session.Event) {
			if _, ok := e.(session.AgentEnd); ok {
				agentEndCount++
			}
		},
		nil,
	)

	if agentEndCount != 1 {
		t.Fatalf("expected exactly 1 AgentEnd on failure, got %d", agentEndCount)
	}
	// Last message should be a failure assistant message.
	last := msgs[len(msgs)-1]
	am, ok := last.(*session.AssistantMessage)
	if !ok {
		t.Fatalf("expected AssistantMessage on failure, got %T", last)
	}
	if am.StopReason != session.StopReasonError {
		t.Fatalf("expected error stop reason, got %q", am.StopReason)
	}
}

// INVARIANT: signal abort causes terminal AgentEnd.
func TestRunLoopAbort(t *testing.T) {
	var agentEndCount int
	signal := make(chan struct{})
	close(signal) // pre-aborted

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "should not complete"},
		}}, nil
	}

	RunLoop(context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}, StreamFn: streamFn},
		func(e session.Event) {
			if _, ok := e.(session.AgentEnd); ok {
				agentEndCount++
			}
		},
		signal,
	)

	if agentEndCount != 1 {
		t.Fatalf("expected exactly 1 AgentEnd on abort, got %d", agentEndCount)
	}
}

// INVARIANT: RunLoop is stateless — no fields, no persistence.
// This is a compile-time invariant enforced by rg guard, but we verify
// the function signature takes all inputs as args.
func TestRunLoopIsStateless(t *testing.T) {
	// Verify RunLoop can be called with no prior state.
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{StopReason: "stop"}}}, nil
	}
	// Each call is independent — no shared state.
	RunLoop(context.Background(), nil, TurnContext{}, LoopConfig{Model: llm.Model{ID: "x"}, StreamFn: streamFn}, func(e session.Event) {}, nil)
	RunLoop(context.Background(), nil, TurnContext{}, LoopConfig{Model: llm.Model{ID: "y"}, StreamFn: streamFn}, func(e session.Event) {}, nil)
}

// INVARIANT: tool calls are executed and ToolExecStart/End emitted.
func TestRunLoopToolExecution(t *testing.T) {
	var toolEvents []session.Event
	callCount := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callCount++
		if callCount == 1 {
			return &mockStream{chunks: []*llm.Chunk{
				{Calls: []llm.Call{{
					ID: "call-1",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "test-tool", Arguments: `{"x":1}`},
				}}, StopReason: "toolUse"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "done", StopReason: "stop"},
		}}, nil
	}

	tool := Tool{
		Name: "test-tool",
		Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{
				ToolCallID: id,
				ToolName:   "test-tool",
				Content:    []session.Content{session.TextContent{Text: "ok"}},
				Timestamp:  time.Now(),
			}, nil
		},
	}

	RunLoop(context.Background(),
		[]session.Message{session.NewUserText("use tool", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}, StreamFn: streamFn, Tools: []Tool{tool}},
		func(e session.Event) { toolEvents = append(toolEvents, e) },
		nil,
	)

	hasStart, hasEnd := false, false
	for _, e := range toolEvents {
		switch e.(type) {
		case session.ToolExecStart:
			hasStart = true
		case session.ToolExecEnd:
			hasEnd = true
		}
	}
	if !hasStart {
		t.Fatal("expected ToolExecStart event")
	}
	if !hasEnd {
		t.Fatal("expected ToolExecEnd event")
	}
}

// TestRunLoopToolPanicRecovery verifies a tool panic is caught and returned as an error result.
func TestRunLoopToolPanicRecovery(t *testing.T) {
	callCount := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callCount++
		if callCount == 1 {
			return &mockStream{chunks: []*llm.Chunk{
				{Calls: []llm.Call{{
					ID: "tc1",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "panic_tool", Arguments: `{}`},
				}}, StopReason: "toolUse"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "recovered", StopReason: "stop"},
		}}, nil
	}

	cfg := LoopConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools: []Tool{{
			Name: "panic_tool",
			Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
				panic("intentional test panic")
			},
		}},
		Convert: DefaultConvert,
	}

	msgs := RunLoop(context.Background(), nil, TurnContext{}, cfg, func(e session.Event) {}, nil)

	foundPanic := false
	for _, m := range msgs {
		switch m := m.(type) {
		case *session.ToolResultMessage:
			for _, c := range m.Content {
				if tc, ok := c.(session.TextContent); ok {
					if strings.Contains(tc.Text, "tool panic") {
						foundPanic = true
					}
				}
			}
		}
	}

	if !foundPanic {
		t.Fatal("expected panic error in tool result message")
	}
}
