package agent

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type mockStream struct {
	chunks []*llm.Chunk
	idx    int
	err    error
}

func (s *mockStream) Next() (*llm.Chunk, bool) {
	if s.idx >= len(s.chunks) {
		return nil, false
	}
	chunk := s.chunks[s.idx]
	s.idx++
	return chunk, true
}

func (s *mockStream) Err() error   { return s.err }
func (s *mockStream) Close() error { return nil }

func TestAgentEventsAndLoop(t *testing.T) {
	var events []session.AgentEvent

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

	cfg := AgentConfig{
		Model:         llm.Model{ID: "test-model"},
		ThinkingLevel: ThinkingLevelMedium,
		StreamFn:      streamFn,
		ToolExecutor:  toolExecutor,
		OnEvent: func(ev session.AgentEvent) {
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
		Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "run test"}},
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
	var gotAgentStart, gotUserMessage, gotTurnStarted, gotThinkingDelta, gotAgentDelta, gotAgentMessage, gotToolCallStarted, gotToolResult, gotTurnEnd bool

	for _, ev := range events {
		switch msg := ev.(type) {
		case session.AgentStart:
			gotAgentStart = true
		case session.UserMessage:
			if msg.Message == "run test" {
				gotUserMessage = true
			}
		case session.TurnStart:
			gotTurnStarted = true
		case session.MessageUpdate:
			if msg.BlockType == "thinking" {
				gotThinkingDelta = true
			} else {
				gotAgentDelta = true
			}
		case session.AgentMessage:
			gotAgentMessage = true
		case session.ToolCallStart:
			gotToolCallStarted = true
		case session.ToolCallEnd:
			gotToolResult = true
		case session.TurnEnd:
			gotTurnEnd = true
		}
	}

	if !gotAgentStart {
		t.Error("missing AgentStart event")
	}
	if !gotUserMessage {
		t.Error("missing UserMessage event")
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
	if !gotTurnEnd {
		t.Error("missing TurnEnd event")
	}
}

func TestAgentRunOwnsPromptUserMessageProjection(t *testing.T) {
	var events []session.AgentEvent
	agent := New(AgentConfig{
		Model: llm.Model{ID: "test-model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "response"}}}, nil
		},
		OnEvent: func(ev session.AgentEvent) {
			events = append(events, ev)
		},
	})

	if _, err := agent.Run(context.Background(), []AgentMessage{{
		Role:    "user",
		Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "project me"}},
	}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if len(events) < 2 {
		t.Fatalf("too few events: %d", len(events))
	}
	msg, ok := events[1].(session.UserMessage)
	if !ok || msg.Message != "project me" {
		t.Fatalf("second event = %#v, want prompt UserMessage after AgentStart", events[1])
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

	adapter := New(AgentConfig{
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

func TestSessionAdapterCancelSettlesWithTurnFinished(t *testing.T) {
	streamEntered := make(chan struct{})
	adapter := New(AgentConfig{
		ID:    "test-session",
		Model: llm.Model{ID: "test-model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			close(streamEntered)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	})

	if err := adapter.Open(context.Background()); err != nil {
		t.Fatalf("open: %v", err)
	}
	if err := adapter.SubmitTurn(context.Background(), "long run"); err != nil {
		t.Fatalf("submit: %v", err)
	}
	select {
	case <-streamEntered:
	case <-time.After(time.Second):
		t.Fatal("stream did not start")
	}
	if err := adapter.CancelTurn(context.Background()); err != nil {
		t.Fatalf("cancel: %v", err)
	}

	var sawFinished bool
	for !sawFinished {
		select {
		case ev := <-adapter.Events():
			switch e := ev.(type) {
			case session.TurnEnd:
				if e.Error != nil {
					t.Fatalf("cancel emitted session error: %#v", ev)
				}
				sawFinished = true
			}
		case <-time.After(time.Second):
			t.Fatal("timed out waiting for cancel terminal event")
		}
	}
}

func TestSessionAdapterSubmitTurnCommitsUserBeforeReturn(t *testing.T) {
	store, err := session.NewEphemeralCantoStore()
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	defer store.Close()

	lazy := session.NewLazySession(store, "/tmp/ion", "model", "main")
	streamEntered := make(chan struct{})
	adapter := New(AgentConfig{
		ID:    lazy.ID(),
		Model: llm.Model{ID: "model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			close(streamEntered)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	})
	adapter.SetSession(lazy)

	if err := adapter.SubmitTurn(context.Background(), "commit first"); err != nil {
		t.Fatalf("submit: %v", err)
	}
	if !session.IsMaterialized(lazy) {
		t.Fatal("lazy session was not materialized before SubmitTurn returned")
	}
	messages, err := lazy.ModelMessages(context.Background())
	if err != nil {
		t.Fatalf("model messages: %v", err)
	}
	if len(messages) != 1 || messages[0].Role != llm.RoleUser ||
		messages[0].TextContent() != "commit first" {
		t.Fatalf("messages = %#v, want committed user prompt", messages)
	}
	select {
	case <-streamEntered:
	case <-time.After(time.Second):
		t.Fatal("stream did not start after submit returned")
	}
	if err := adapter.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
}

func TestSessionAdapterDrainsQueuedFollowUpsOneAtATime(t *testing.T) {
	firstStreamEntered := make(chan struct{})
	releaseFirstStream := make(chan struct{})
	var closeFirstOnce sync.Once
	adapter := New(AgentConfig{
		ID:    "test-session",
		Model: llm.Model{ID: "model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			if len(req.Messages) == 1 {
				closeFirstOnce.Do(func() { close(firstStreamEntered) })
				select {
				case <-releaseFirstStream:
				case <-ctx.Done():
					return nil, ctx.Err()
				}
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "response"}}}, nil
		},
	})

	if err := adapter.SubmitTurn(context.Background(), "initial"); err != nil {
		t.Fatalf("submit: %v", err)
	}
	select {
	case <-firstStreamEntered:
	case <-time.After(time.Second):
		t.Fatal("first stream did not start")
	}
	if _, err := adapter.FollowUpTurn(context.Background(), "follow one"); err != nil {
		t.Fatalf("first follow-up: %v", err)
	}
	if _, err := adapter.FollowUpTurn(context.Background(), "follow two"); err != nil {
		t.Fatalf("second follow-up: %v", err)
	}
	close(releaseFirstStream)

	var users []string
	for {
		select {
		case ev := <-adapter.Events():
			switch msg := ev.(type) {
			case session.UserMessage:
				users = append(users, msg.Message)
			case session.AgentEnd:
				want := []string{"initial", "follow one", "follow two"}
				if strings.Join(users, ",") != strings.Join(want, ",") {
					t.Fatalf("user events = %v, want %v", users, want)
				}
				return
			}
		case <-time.After(time.Second):
			t.Fatalf("timed out waiting for terminal event; users = %v", users)
		}
	}
}

func TestAgentSystemPromptPropagation(t *testing.T) {
	var observedReq *llm.Request
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		observedReq = req
		return &mockStream{chunks: []*llm.Chunk{{Content: "response"}}}, nil
	}

	modelCaps := &llm.Capabilities{
		SystemRole: "developer",
	}

	cfg := AgentConfig{
		Model: llm.Model{
			ID:           "test-reasoning-model",
			Capabilities: modelCaps,
		},
		StreamFn: streamFn,
	}

	agent := New(cfg)
	agent.SetSystemPrompt("durable instruction set")

	userMsg := AgentMessage{
		Role:    "user",
		Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hello"}},
	}

	_, err := agent.Run(context.Background(), []AgentMessage{userMsg})
	if err != nil {
		t.Fatalf("agent run: %v", err)
	}

	if observedReq == nil {
		t.Fatal("expected LLM stream function to be invoked")
	}

	if len(observedReq.Messages) != 2 {
		t.Fatalf("expected 2 messages sent to LLM, got %d", len(observedReq.Messages))
	}

	sysMsg := observedReq.Messages[0]
	if sysMsg.Role != "developer" {
		t.Errorf("expected system message role to be 'developer', got %q", sysMsg.Role)
	}
	if sysMsg.TextContent() != "durable instruction set" {
		t.Errorf(
			"expected system message content to be 'durable instruction set', got %q",
			sysMsg.TextContent(),
		)
	}

	userMsgOut := observedReq.Messages[1]
	if userMsgOut.Role != "user" {
		t.Errorf("expected second message role to be 'user', got %q", userMsgOut.Role)
	}
	if userMsgOut.TextContent() != "hello" {
		t.Errorf("expected second message content to be 'hello', got %q", userMsgOut.TextContent())
	}
}

func TestAgentToolsIncludedInRequest(t *testing.T) {
	var observedReq *llm.Request
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		observedReq = req
		return &mockStream{chunks: []*llm.Chunk{{Content: "no tools needed"}}}, nil
	}

	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test-model"},
		StreamFn: streamFn,
	})
	agent.SetTools([]AgentTool{
		{Name: "read", Description: "Read a file", Parameters: map[string]any{"type": "object"}},
		{Name: "bash", Description: "Run a command", Parameters: map[string]any{"type": "object"}},
	})

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hello"}}}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if observedReq == nil {
		t.Fatal("stream function not called")
	}
	if len(observedReq.Tools) != 2 {
		t.Fatalf("tools in request = %d, want 2", len(observedReq.Tools))
	}
	if observedReq.Tools[0].Name != "read" {
		t.Errorf("tools[0].name = %q, want read", observedReq.Tools[0].Name)
	}
	if observedReq.Tools[1].Name != "bash" {
		t.Errorf("tools[1].name = %q, want bash", observedReq.Tools[1].Name)
	}
	if observedReq.Tools[0].Description != "Read a file" {
		t.Errorf("tools[0].description = %q", observedReq.Tools[0].Description)
	}
}

func TestAgentNoToolsOmitsToolsField(t *testing.T) {
	var observedReq *llm.Request
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		observedReq = req
		return &mockStream{chunks: []*llm.Chunk{{Content: "ok"}}}, nil
	}

	agent := New(AgentConfig{
		Model:    llm.Model{ID: "test-model"},
		StreamFn: streamFn,
	})

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hello"}}}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if observedReq == nil {
		t.Fatal("stream function not called")
	}
	if len(observedReq.Tools) != 0 {
		t.Fatalf("tools in request = %d, want 0 (no tools registered)", len(observedReq.Tools))
	}
}

func TestAgentValidatesRequiredToolArgs(t *testing.T) {
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "read", `{"offset":5}`)},
		}}}, nil
	}

	var toolError string
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, tc AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("ok")}}, nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return true
		},
	})
	agent.SetTools([]AgentTool{{
		Name:       "read",
		Parameters: map[string]any{"required": []any{"path"}},
	}})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	// The tool should NOT have been called because 'path' is missing
	if toolError != "" {
		t.Fatalf("tool was called despite missing required arg: %s", toolError)
	}
}

func TestAgentAllowsToolCallWithAllRequiredArgs(t *testing.T) {
	var toolCalled bool
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "read", `{"path":"README.md"}`)},
		}}}, nil
	}

	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, tc AgentToolCall) (AgentToolResult, error) {
			toolCalled = true
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("ok")}}, nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return true
		},
	})
	agent.SetTools([]AgentTool{{
		Name:       "read",
		Parameters: map[string]any{"required": []any{"path"}},
	}})

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read"}}}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if !toolCalled {
		t.Fatal("tool was not called despite having all required args")
	}
}

func TestAgentValidatesPropertyTypes(t *testing.T) {
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "read", `{"path":123}`)},
		}}}, nil
	}

	var toolCalled bool
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, tc AgentToolCall) (AgentToolResult, error) {
			toolCalled = true
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("ok")}}, nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return true
		},
	})
	agent.SetTools([]AgentTool{{
		Name:       "read",
		Parameters: map[string]any{"required": []any{"path"}, "properties": map[string]any{"path": map[string]any{"type": "string"}}},
	}})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if toolCalled {
		t.Fatal("tool was called despite wrong type for 'path' (number instead of string)")
	}
}

func TestAgentPrepareNextTurnAndToolHookContext(t *testing.T) {
	var requests []string
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		requests = append(requests, req.Model)
		if len(requests) == 1 {
			return &mockStream{chunks: []*llm.Chunk{{
				Content: "need tool",
				Calls:   []llm.Call{testCall("call-1", "read", `{"path":"README.md"}`)},
			}}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{{Content: "done"}}}, nil
	}

	var before BeforeToolCallContext
	var after AfterToolCallContext
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "first"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("contents")}}, nil
		},
		BeforeToolCall: func(ctx context.Context, hookCtx BeforeToolCallContext) BeforeToolCallResult {
			before = hookCtx
			return BeforeToolCallResult{}
		},
		AfterToolCall: func(ctx context.Context, hookCtx AfterToolCallContext) AfterToolCallResult {
			after = hookCtx
			return AfterToolCallResult{}
		},
		PrepareNextTurn: func(ctx PrepareNextTurnContext) *AgentLoopTurnUpdate {
			if len(ctx.ToolResults) == 0 {
				return nil
			}
			next := llm.Model{ID: "second"}
			return &AgentLoopTurnUpdate{Model: &next}
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(requests) >= 2
		},
	})
	agent.SetTools([]AgentTool{{Name: "read"}})

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "go"}}}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if got, want := strings.Join(requests, ","), "first,second"; got != want {
		t.Fatalf("request models = %s, want %s", got, want)
	}
	if before.AssistantMessage.TextContent() != "need tool" {
		t.Fatalf("before assistant = %#v", before.AssistantMessage)
	}
	if before.Args == nil || before.ToolCall.Arguments["path"] != "README.md" {
		t.Fatalf("before args = %#v", before)
	}
	if after.Result.Content[0].Text != "contents" || after.Args == nil {
		t.Fatalf("after context = %#v", after)
	}
}

func TestBeforeToolCallBlocksExecution(t *testing.T) {
	var toolExecuted bool
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "read", `{"path": "README.md"}`)},
		}}}, nil
	}
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			toolExecuted = true
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("contents")}}, nil
		},
		BeforeToolCall: func(ctx context.Context, hookCtx BeforeToolCallContext) BeforeToolCallResult {
			return BeforeToolCallResult{Block: true, Reason: "blocked by policy"}
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return true
		},
	})
	agent.SetTools([]AgentTool{{Name: "read"}})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read file"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if toolExecuted {
		t.Fatal("tool should not have executed when blocked by BeforeToolCall")
	}
}

func TestBeforeToolCallBlockDefaultReason(t *testing.T) {
	var blockedReason string
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "read", `{"path": "README.md"}`)},
		}}}, nil
	}
	committed := make([]llm.Message, 0)
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("contents")}}, nil
		},
		BeforeToolCall: func(ctx context.Context, hookCtx BeforeToolCallContext) BeforeToolCallResult {
			return BeforeToolCallResult{Block: true}
		},
		OnModelMessage: func(ctx context.Context, message llm.Message) error {
			committed = append(committed, message)
			return nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(committed) >= 2
		},
	})
	agent.SetTools([]AgentTool{{Name: "read"}})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read file"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	// Find the tool result message
	for _, msg := range committed {
		if msg.Role == llm.RoleTool {
			blockedReason = msg.TextContent()
			break
		}
	}
	if !strings.Contains(blockedReason, "Tool execution was blocked") {
		t.Fatalf("expected default block reason, got: %q", blockedReason)
	}
}

func TestAfterToolCallMutatesResult(t *testing.T) {
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "read", `{"path": "README.md"}`)},
		}}}, nil
	}
	committed := make([]llm.Message, 0)
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("original")}}, nil
		},
		AfterToolCall: func(ctx context.Context, hookCtx AfterToolCallContext) AfterToolCallResult {
			return AfterToolCallResult{
				Content: []llm.ContentPart{llm.TextPart("mutated")},
			}
		},
		OnModelMessage: func(ctx context.Context, message llm.Message) error {
			committed = append(committed, message)
			return nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(committed) >= 2
		},
	})
	agent.SetTools([]AgentTool{{Name: "read"}})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read file"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	// Find the tool result message
	for _, msg := range committed {
		if msg.Role == llm.RoleTool {
			if !strings.Contains(msg.TextContent(), "mutated") {
				t.Fatalf("expected mutated content, got: %q", msg.TextContent())
			}
			return
		}
	}
	t.Fatal("missing tool result message")
}

func TestAfterToolCallSetsTerminate(t *testing.T) {
	var toolCalls int
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		toolCalls++
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "read", `{"path": "README.md"}`)},
		}}}, nil
	}
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("contents")}}, nil
		},
		AfterToolCall: func(ctx context.Context, hookCtx AfterToolCallContext) AfterToolCallResult {
			terminate := true
			return AfterToolCallResult{Terminate: &terminate}
		},
	})
	agent.SetTools([]AgentTool{{Name: "read"}})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read file"}}}})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if toolCalls != 1 {
		t.Fatalf("expected 1 tool call (terminate after first), got: %d", toolCalls)
	}
}

func TestAgentPreservesStructuredToolResultParts(t *testing.T) {
	var committed []llm.Message
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		if len(committed) < 3 {
			return &mockStream{chunks: []*llm.Chunk{{
				Calls: []llm.Call{testCall("call-1", "read", `{}`)},
			}}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{{Content: "done"}}}, nil
	}
	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{
				llm.TextPart("image result\n"),
				llm.ImagePart("image/png", "abc123"),
			}}, nil
		},
		OnModelMessage: func(ctx context.Context, message llm.Message) error {
			committed = append(committed, message)
			return nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(committed) >= 4
		},
	})
	agent.SetTools([]AgentTool{{Name: "read"}})

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "read image"}}}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	var toolMsg *llm.Message
	for i := range committed {
		if committed[i].Role == llm.RoleTool {
			toolMsg = &committed[i]
			break
		}
	}
	if toolMsg == nil {
		t.Fatal("missing committed tool message")
	}
	if len(toolMsg.Parts) != 2 || toolMsg.Parts[1].Type != llm.ContentPartImage {
		t.Fatalf("tool parts = %#v", toolMsg.Parts)
	}
	if !strings.Contains(toolMsg.TextContent(), "image result") {
		t.Fatalf("tool content = %q, want image result", toolMsg.TextContent())
	}
}

func TestAgentParallelToolsEmitLifecycleInSourceOrder(t *testing.T) {
	releaseFirst := make(chan struct{})
	secondRan := make(chan struct{})
	errc := make(chan error, 1)
	var (
		mu        sync.Mutex
		lifecycle []string
	)
	agent := New(AgentConfig{
		Model:             llm.Model{ID: "model"},
		ToolExecutionMode: ToolExecutionParallel,
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{
				Calls: []llm.Call{
					testCall("call-1", "first", `{}`),
					testCall("call-2", "second", `{}`),
				},
			}}}, nil
		},
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			switch toolCall.Name {
			case "first":
				<-releaseFirst
				return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("first done")}}, nil
			case "second":
				close(secondRan)
				return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("second done")}}, nil
			default:
				return AgentToolResult{}, errors.New("unexpected tool")
			}
		},
		OnEvent: func(ev session.AgentEvent) {
			mu.Lock()
			defer mu.Unlock()
			switch msg := ev.(type) {
			case session.ToolCallStart:
				lifecycle = append(lifecycle, "start:"+msg.ToolName)
			case session.ToolCallEnd:
				lifecycle = append(lifecycle, "result:"+msg.ToolName)
			}
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(ctx.ToolResults) == 2
		},
	})
	agent.SetTools([]AgentTool{
		{Name: "first", ExecutionMode: ToolExecutionParallel},
		{Name: "second", ExecutionMode: ToolExecutionParallel},
	})

	go func() {
		_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "go"}}}})
		errc <- err
	}()

	select {
	case <-secondRan:
	case <-time.After(time.Second):
		t.Fatal("second parallel tool did not run while first was blocked")
	}
	close(releaseFirst)

	select {
	case err := <-errc:
		if err != nil {
			t.Fatalf("run: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("agent run did not finish")
	}

	mu.Lock()
	defer mu.Unlock()
	want := []string{"start:first", "start:second", "result:first", "result:second"}
	if strings.Join(lifecycle, ",") != strings.Join(want, ",") {
		t.Fatalf("lifecycle = %v, want %v", lifecycle, want)
	}
}

func TestAgentParallelPreflightSequentialAndFinalizeConcurrent(t *testing.T) {
	var (
		mu        sync.Mutex
		preflight []string
		finalized []string
	)

	releaseFirst := make(chan struct{})
	secondRan := make(chan struct{})

	agent := New(AgentConfig{
		Model:             llm.Model{ID: "model"},
		ToolExecutionMode: ToolExecutionParallel,
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{
				Calls: []llm.Call{
					testCall("call-1", "first", `{}`),
					testCall("call-2", "second", `{}`),
				},
			}}}, nil
		},
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			switch toolCall.Name {
			case "first":
				// Block first tool execution until second ran
				<-releaseFirst
				return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("first done")}}, nil
			case "second":
				close(secondRan)
				return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("second done")}}, nil
			default:
				return AgentToolResult{}, errors.New("unexpected tool")
			}
		},
		BeforeToolCall: func(ctx context.Context, hookCtx BeforeToolCallContext) BeforeToolCallResult {
			mu.Lock()
			preflight = append(preflight, hookCtx.ToolCall.Name)
			mu.Unlock()
			return BeforeToolCallResult{}
		},
		AfterToolCall: func(ctx context.Context, hookCtx AfterToolCallContext) AfterToolCallResult {
			mu.Lock()
			finalized = append(finalized, hookCtx.ToolCall.Name)
			mu.Unlock()
			return AfterToolCallResult{}
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(ctx.ToolResults) == 2
		},
	})

	agent.SetTools([]AgentTool{
		{Name: "first", ExecutionMode: ToolExecutionParallel},
		{Name: "second", ExecutionMode: ToolExecutionParallel},
	})

	errc := make(chan error, 1)
	go func() {
		_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "go"}}}})
		errc <- err
	}()

	select {
	case <-secondRan:
	case <-time.After(time.Second):
		t.Fatal("second parallel tool did not run while first was blocked")
	}

	// Since AfterToolCall runs concurrently, second tool must have finished and appended to finalized
	// before the first tool is released!
	mu.Lock()
	if len(finalized) != 1 || finalized[0] != "second" {
		mu.Unlock()
		t.Fatalf("expected only 'second' to be finalized, got %v", finalized)
	}
	mu.Unlock()

	close(releaseFirst)

	select {
	case err := <-errc:
		if err != nil {
			t.Fatalf("run: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("agent run did not finish")
	}

	mu.Lock()
	defer mu.Unlock()

	// Verify sequential preflight order
	if len(preflight) != 2 || preflight[0] != "first" || preflight[1] != "second" {
		t.Errorf("preflight hook order incorrect: %v", preflight)
	}

	// Verify concurrent/completion-order finalized order (second was faster than first)
	if len(finalized) != 2 || finalized[0] != "second" || finalized[1] != "first" {
		t.Errorf("finalized hook order incorrect (expected second, then first): %v", finalized)
	}
}

func TestAgentPerToolExecutionModeOverridesGlobal(t *testing.T) {
	// Global config is Parallel, but one tool has ExecutionMode=Sequential.
	// The sequential tool should force sequential execution.
	toolOrder := make(chan string, 10)

	agent := New(AgentConfig{
		Model:             llm.Model{ID: "model"},
		ToolExecutionMode: ToolExecutionParallel,
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{
				Calls: []llm.Call{
					testCall("call-1", "fast", `{}`),
					testCall("call-2", "slow", `{}`),
				},
			}}}, nil
		},
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			toolOrder <- toolCall.Name
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart(toolCall.Name + " done")}}, nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(ctx.ToolResults) == 2
		},
	})
	// slow tool overrides to Sequential
	agent.SetTools([]AgentTool{
		{Name: "fast", ExecutionMode: ToolExecutionParallel},
		{Name: "slow", ExecutionMode: ToolExecutionSequential},
	})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "go"}}}})
	if err != nil {
		t.Fatalf("Run failed: %v", err)
	}

	// Both tools should have executed
	close(toolOrder)
	var order []string
	for name := range toolOrder {
		order = append(order, name)
	}
	if len(order) != 2 {
		t.Fatalf("expected 2 tool executions, got %d: %v", len(order), order)
	}
}

func TestAgentConsumedFollowUpEmitsUserMessage(t *testing.T) {
	var events []session.AgentEvent
	var committed []llm.Message
	requests := 0
	followUpSent := false
	agent := New(AgentConfig{
		Model: llm.Model{ID: "model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			requests++
			return &mockStream{chunks: []*llm.Chunk{{Content: "response"}}}, nil
		},
		GetFollowUpMessages: func() []AgentMessage {
			if followUpSent {
				return nil
			}
			followUpSent = true
			return []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "queued follow-up"}}}}
		},
		OnEvent: func(ev session.AgentEvent) {
			events = append(events, ev)
		},
		OnModelMessage: func(ctx context.Context, message llm.Message) error {
			committed = append(committed, message)
			return nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return requests >= 2
		},
	})

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "initial"}}}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	var sawFollowUpEvent bool
	for _, ev := range events {
		msg, ok := ev.(session.UserMessage)
		if ok && msg.Message == "queued follow-up" {
			sawFollowUpEvent = true
			break
		}
	}
	if !sawFollowUpEvent {
		t.Fatalf("events = %#v, want queued follow-up user event", events)
	}
	var sawFollowUpCommit bool
	for _, message := range committed {
		if message.Role == llm.RoleUser && message.TextContent() == "queued follow-up" {
			sawFollowUpCommit = true
			break
		}
	}
	if !sawFollowUpCommit {
		t.Fatalf("committed = %#v, want queued follow-up model message", committed)
	}
}

func TestAgentStreamErrorDoesNotCommitAssistantMessage(t *testing.T) {
	var events []session.AgentEvent
	var committed []llm.Message
	streamErr := errors.New("provider stream failed")
	agent := New(AgentConfig{
		Model: llm.Model{ID: "model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{
				chunks: []*llm.Chunk{{Content: "partial response"}},
				err:    streamErr,
			}, nil
		},
		OnEvent: func(ev session.AgentEvent) {
			events = append(events, ev)
		},
		OnModelMessage: func(ctx context.Context, message llm.Message) error {
			committed = append(committed, message)
			return nil
		},
	})

	messages, err := agent.Run(context.Background(), []AgentMessage{{
		Role:    "user",
		Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hello"}},
	}})
	if err == nil {
		t.Fatal("run succeeded after stream error")
	}
	if !strings.Contains(err.Error(), streamErr.Error()) {
		t.Fatalf("run error = %v, want stream error", err)
	}
	if len(messages) != 1 || messages[0].Role != "user" {
		t.Fatalf("new messages = %#v, want only user prompt", messages)
	}
	if len(committed) != 1 || committed[0].Role != llm.RoleUser {
		t.Fatalf("committed messages = %#v, want only user prompt", committed)
	}
	for _, ev := range events {
		if _, ok := ev.(session.AgentMessage); ok {
			t.Fatalf("unexpected assistant message event after stream error: %#v", ev)
		}
	}
}

func TestSessionAdapterResumeHydratesModelHistory(t *testing.T) {
	store, err := session.NewEphemeralCantoStore()
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	defer store.Close()

	sess, err := store.OpenSession(context.Background(), "/tmp/ion", "model", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	writer := sess.(interface {
		AppendModelMessage(context.Context, llm.Message) error
	})
	if err := writer.AppendModelMessage(context.Background(), llm.TextMessage(llm.RoleUser, "prior")); err != nil {
		t.Fatalf("append user: %v", err)
	}
	if err := writer.AppendModelMessage(context.Background(), llm.TextMessage(llm.RoleAssistant, "answer")); err != nil {
		t.Fatalf("append assistant: %v", err)
	}

	adapter := New(AgentConfig{
		ID:    "placeholder",
		Model: llm.Model{ID: "model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "next"}}}, nil
		},
	})
	adapter.SetSession(sess)
	if err := adapter.Resume(context.Background(), sess.ID()); err != nil {
		t.Fatalf("resume: %v", err)
	}
	messages := adapter.State().Messages
	if len(messages) != 2 {
		t.Fatalf("messages = %#v", messages)
	}
	if messages[0].TextContent() != "prior" || messages[1].TextContent() != "answer" {
		t.Fatalf("hydrated messages = %#v", messages)
	}
}

func testCall(id, name, args string) llm.Call {
	var call llm.Call
	call.ID = id
	call.Type = "function"
	call.Function.Name = name
	call.Function.Arguments = args
	return call
}

func TestAgentPrepareArguments(t *testing.T) {
	var capturedArgs map[string]any

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{
			Calls: []llm.Call{testCall("call-1", "test_tool", `{"path": "relative.txt"}`)},
		}}}, nil
	}

	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, tc AgentToolCall) (AgentToolResult, error) {
			capturedArgs = tc.Arguments
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("ok")}}, nil
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return true
		},
	})
	agent.SetTools([]AgentTool{{
		Name:       "test_tool",
		Parameters: map[string]any{"type": "object", "properties": map[string]any{"path": map[string]any{"type": "string"}}},
		PrepareArguments: func(args map[string]any) map[string]any {
			if p, ok := args["path"].(string); ok {
				args["path"] = "/workspace/" + p
			}
			return args
		},
	}})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "test"}}}})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if capturedArgs == nil {
		t.Fatal("tool was not called")
	}
	if p, ok := capturedArgs["path"].(string); !ok || p != "/workspace/relative.txt" {
		t.Errorf("path = %v, want /workspace/relative.txt", capturedArgs["path"])
	}
}

func TestTransformContextModifiesMessages(t *testing.T) {
	var capturedRequest *llm.Request
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		capturedRequest = req
		return &mockStream{chunks: []*llm.Chunk{{Content: "done"}}}, nil
	}

	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		TransformContext: func(ctx context.Context, messages []AgentMessage) []AgentMessage {
			// Add a system message
			return append([]AgentMessage{{Role: "system", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "transformed"}}}}, messages...)
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return true
		},
	})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "test"}}}})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if capturedRequest == nil {
		t.Fatal("no request captured")
	}
	if len(capturedRequest.Messages) == 0 {
		t.Fatal("empty messages")
	}
	// Check that the first message is the transformed system message
	if capturedRequest.Messages[0].Role != "system" {
		t.Fatalf("first message role = %q, want system", capturedRequest.Messages[0].Role)
	}
}

func TestHandleRunFailureCalledOnError(t *testing.T) {
	var failureErr error
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return nil, fmt.Errorf("stream error")
	}

	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		HandleRunFailure: func(err error) {
			failureErr = err
		},
	})

	// Direct Run doesn't call HandleRunFailure (only background turns do)
	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "test"}}}})
	if err == nil {
		t.Fatal("expected error")
	}
	if failureErr != nil {
		t.Fatal("HandleRunFailure should not be called on direct Run")
	}
}

func TestHandleRunFailureNotCalledOnSuccess(t *testing.T) {
	var called bool
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{Content: "done"}}}, nil
	}

	agent := New(AgentConfig{
		Model:    llm.Model{ID: "model"},
		StreamFn: streamFn,
		HandleRunFailure: func(err error) {
			called = true
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return true
		},
	})

	_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "test"}}}})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if called {
		t.Fatal("HandleRunFailure should not be called on success")
	}
}
