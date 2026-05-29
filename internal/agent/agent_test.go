package agent

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/llm"
)

type mockStream struct {
	chunks []*llm.Chunk
	idx    int
}

func (s *mockStream) Next() (*llm.Chunk, bool) {
	if s.idx >= len(s.chunks) {
		return nil, false
	}
	chunk := s.chunks[s.idx]
	s.idx++
	return chunk, true
}

func (s *mockStream) Err() error   { return nil }
func (s *mockStream) Close() error { return nil }

func TestAgentEventsAndLoop(t *testing.T) {
	var events []session.Event

	chunks := []*llm.Chunk{
		{Reasoning: "thinking..."},
		{Content: "Hello "},
		{Content: "world!"},
		{
			Calls: []llm.Call{
				{
					ID:   "call-1",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{
						Name:      "test-tool",
						Arguments: `{"param":"val"}`,
					},
				},
			},
		},
	}

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: chunks}, nil
	}

	toolExecutor := func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
		if toolCall.Name != "test-tool" {
			return AgentToolResult{}, errors.New("unknown tool")
		}
		if toolCall.Arguments["param"] != "val" {
			return AgentToolResult{}, errors.New("invalid argument")
		}
		return AgentToolResult{
			Content: []llm.ContentPart{llm.TextPart("tool success")},
		}, nil
	}

	cfg := AgentLoopConfig{
		Model:         llm.Model{ID: "test-model"},
		ThinkingLevel: ThinkingLevelMedium,
		StreamFn:      streamFn,
		ToolExecutor:  toolExecutor,
		OnEvent: func(ev session.Event) {
			events = append(events, ev)
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			// Stop after the second turn (when tool result has been submitted)
			return len(ctx.NewMessages) >= 3
		},
	}

	agent := New(cfg)
	agent.SetTools([]AgentTool{
		{
			Name:        "test-tool",
			Description: "A mock tool",
			Parameters:  nil,
		},
	})

	userMsg := AgentMessage{
		Role:    "user",
		Content: "run test",
	}

	newMsgs, err := agent.Run(context.Background(), []AgentMessage{userMsg})
	if err != nil {
		t.Fatalf("agent run: %v", err)
	}

	// Verify messages returned
	if len(newMsgs) < 3 {
		t.Fatalf("expected at least 3 messages, got %d", len(newMsgs))
	}

	// Verify event sequence
	var gotTurnStarted, gotThinkingDelta, gotAgentDelta, gotAgentMessage, gotToolCallStarted, gotToolResult, gotTurnFinished bool

	for _, ev := range events {
		switch ev.(type) {
		case session.TurnStarted:
			gotTurnStarted = true
		case session.ThinkingDelta:
			gotThinkingDelta = true
		case session.AgentDelta:
			gotAgentDelta = true
		case session.AgentMessage:
			gotAgentMessage = true
		case session.ToolCallStarted:
			gotToolCallStarted = true
		case session.ToolResult:
			gotToolResult = true
		case session.TurnFinished:
			gotTurnFinished = true
		}
	}

	if !gotTurnStarted {
		t.Error("missing TurnStarted event")
	}
	if !gotThinkingDelta {
		t.Error("missing ThinkingDelta event")
	}
	if !gotAgentDelta {
		t.Error("missing AgentDelta event")
	}
	if !gotAgentMessage {
		t.Error("missing AgentMessage event")
	}
	if !gotToolCallStarted {
		t.Error("missing ToolCallStarted event")
	}
	if !gotToolResult {
		t.Error("missing ToolResult event")
	}
	if !gotTurnFinished {
		t.Error("missing TurnFinished event")
	}
}

func TestJSONHelpers(t *testing.T) {
	args := `{"foo":"bar","num":42}`
	parsed := parseArguments(args)
	if parsed["foo"] != "bar" || parsed["num"] != 42.0 {
		t.Errorf("parsed arguments incorrect: %v", parsed)
	}

	serialized := serializeArguments(parsed)
	if !strings.Contains(serialized, `"foo":"bar"`) || !strings.Contains(serialized, `"num":42`) {
		t.Errorf("serialized arguments incorrect: %s", serialized)
	}

	// Fallback check
	rawParsed := parseArguments(`invalid-json`)
	if rawParsed["raw"] != "invalid-json" {
		t.Errorf("invalid json did not parse to raw field: %v", rawParsed)
	}

	rawSerialized := serializeArguments(rawParsed)
	if rawSerialized != "invalid-json" {
		t.Errorf("raw argument did not serialize correctly: %s", rawSerialized)
	}
}

func TestSessionAdapterQueuesAndCancel(t *testing.T) {
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{Content: "response"}}}, nil
	}

	adapter := NewSessionAdapter(&SessionAdapterConfig{
		ID:       "test-session",
		Model:    llm.Model{ID: "test-model"},
		StreamFn: streamFn,
	})

	ctx := context.Background()
	err := adapter.Open(ctx)
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	// Test Steering
	res, err := adapter.SteerTurn(ctx, "steering info")
	if err != nil {
		t.Fatalf("steer: %v", err)
	}
	if res.Outcome != session.SteeringAccepted {
		t.Errorf("expected outcome accepted, got %v", res.Outcome)
	}

	// Test FollowUp
	qres, err := adapter.FollowUpTurn(ctx, "follow-up message")
	if err != nil {
		t.Fatalf("follow-up: %v", err)
	}
	if qres.Outcome != session.QueuedInputAccepted {
		t.Errorf("expected queued input accepted, got %v", qres.Outcome)
	}

	// Verify clears
	snapshot, err := adapter.ClearQueuedInput(ctx)
	if err != nil {
		t.Fatalf("clear: %v", err)
	}
	if len(snapshot.Steering) != 1 || snapshot.Steering[0] != "steering info" {
		t.Errorf("expected 1 steering entry 'steering info', got %v", snapshot.Steering)
	}
	if len(snapshot.FollowUp) != 1 || snapshot.FollowUp[0] != "follow-up message" {
		t.Errorf("expected 1 follow-up entry 'follow-up message', got %v", snapshot.FollowUp)
	}

	// Test Cancel
	cancelCtx, cancel := context.WithCancel(context.Background())
	err = adapter.SubmitTurn(cancelCtx, "long run")
	if err != nil {
		t.Fatalf("submit: %v", err)
	}

	err = adapter.CancelTurn(ctx)
	if err != nil {
		t.Fatalf("cancel: %v", err)
	}
	cancel()

	err = adapter.Close()
	if err != nil {
		t.Fatalf("close: %v", err)
	}
}

