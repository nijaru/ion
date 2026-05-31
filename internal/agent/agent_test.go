package agent

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/llm"
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
	var gotUserMessage, gotTurnStarted, gotThinkingDelta, gotAgentDelta, gotAgentMessage, gotToolCallStarted, gotToolResult, gotTurnFinished bool

	for _, ev := range events {
		switch msg := ev.(type) {
		case session.UserMessage:
			if msg.Message == "run test" {
				gotUserMessage = true
			}
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
	if !gotTurnFinished {
		t.Error("missing TurnFinished event")
	}
}

func TestAgentRunOwnsPromptUserMessageProjection(t *testing.T) {
	var events []session.Event
	agent := New(AgentLoopConfig{
		Model: llm.Model{ID: "test-model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "response"}}}, nil
		},
		OnEvent: func(ev session.Event) {
			events = append(events, ev)
		},
	})

	if _, err := agent.Run(context.Background(), []AgentMessage{{
		Role:    "user",
		Content: "project me",
	}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if len(events) == 0 {
		t.Fatal("missing events")
	}
	msg, ok := events[0].(session.UserMessage)
	if !ok || msg.Message != "project me" {
		t.Fatalf("first event = %#v, want prompt UserMessage", events[0])
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

func TestSessionAdapterCancelSettlesWithTurnFinished(t *testing.T) {
	streamEntered := make(chan struct{})
	adapter := NewSessionAdapter(&SessionAdapterConfig{
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
			switch ev.(type) {
			case session.Error:
				t.Fatalf("cancel emitted session error: %#v", ev)
			case session.TurnFinished:
				sawFinished = true
			}
		case <-time.After(time.Second):
			t.Fatal("timed out waiting for cancel terminal event")
		}
	}
}

func TestSessionAdapterSubmitTurnCommitsUserBeforeReturn(t *testing.T) {
	store, err := storage.NewEphemeralCantoStore()
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	defer store.Close()

	lazy := storage.NewLazySession(store, "/tmp/ion", "model", "main")
	streamEntered := make(chan struct{})
	adapter := NewSessionAdapter(&SessionAdapterConfig{
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
	if !storage.IsMaterialized(lazy) {
		t.Fatal("lazy session was not materialized before SubmitTurn returned")
	}
	messages, err := lazy.ModelMessages(context.Background())
	if err != nil {
		t.Fatalf("model messages: %v", err)
	}
	if len(messages) != 1 || messages[0].Role != llm.RoleUser ||
		messages[0].Content != "commit first" {
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

func TestAgentSystemPromptPropagation(t *testing.T) {
	var observedReq *llm.Request
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		observedReq = req
		return &mockStream{chunks: []*llm.Chunk{{Content: "response"}}}, nil
	}

	modelCaps := &llm.Capabilities{
		SystemRole: "developer",
	}

	cfg := AgentLoopConfig{
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
		Content: "hello",
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
	if sysMsg.Content != "durable instruction set" {
		t.Errorf(
			"expected system message content to be 'durable instruction set', got %q",
			sysMsg.Content,
		)
	}

	userMsgOut := observedReq.Messages[1]
	if userMsgOut.Role != "user" {
		t.Errorf("expected second message role to be 'user', got %q", userMsgOut.Role)
	}
	if userMsgOut.Content != "hello" {
		t.Errorf("expected second message content to be 'hello', got %q", userMsgOut.Content)
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
	agent := New(AgentLoopConfig{
		Model:    llm.Model{ID: "first"},
		StreamFn: streamFn,
		ToolExecutor: func(ctx context.Context, toolCall AgentToolCall) (AgentToolResult, error) {
			return AgentToolResult{Content: []llm.ContentPart{llm.TextPart("contents")}}, nil
		},
		BeforeToolCall: func(ctx BeforeToolCallContext) BeforeToolCallResult {
			before = ctx
			return BeforeToolCallResult{}
		},
		AfterToolCall: func(ctx AfterToolCallContext) AfterToolCallResult {
			after = ctx
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

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Content: "go"}}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if got, want := strings.Join(requests, ","), "first,second"; got != want {
		t.Fatalf("request models = %s, want %s", got, want)
	}
	if before.AssistantMessage.Content != "need tool" {
		t.Fatalf("before assistant = %#v", before.AssistantMessage)
	}
	if before.Args == nil || before.ToolCall.Arguments["path"] != "README.md" {
		t.Fatalf("before args = %#v", before)
	}
	if after.Result.Content[0].Text != "contents" || after.Args == nil {
		t.Fatalf("after context = %#v", after)
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
	agent := New(AgentLoopConfig{
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

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Content: "read image"}}); err != nil {
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
	if !strings.Contains(toolMsg.Content, "Image: image/png") {
		t.Fatalf("tool content = %q, want image notice", toolMsg.Content)
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
	agent := New(AgentLoopConfig{
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
		OnEvent: func(ev session.Event) {
			mu.Lock()
			defer mu.Unlock()
			switch msg := ev.(type) {
			case session.ToolCallStarted:
				lifecycle = append(lifecycle, "start:"+msg.ToolName)
			case session.ToolResult:
				lifecycle = append(lifecycle, "result:"+msg.ToolName)
			}
		},
		ShouldStopAfterTurn: func(ctx ShouldStopAfterTurnContext) bool {
			return len(ctx.ToolResults) == 2
		},
	})
	agent.SetTools([]AgentTool{
		{Name: "first", Parallel: true},
		{Name: "second", Parallel: true},
	})

	go func() {
		_, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Content: "go"}})
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

func TestAgentConsumedFollowUpEmitsUserMessageEvent(t *testing.T) {
	var events []session.Event
	var committed []llm.Message
	requests := 0
	followUpSent := false
	agent := New(AgentLoopConfig{
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
			return []AgentMessage{{Role: "user", Content: "queued follow-up"}}
		},
		OnEvent: func(ev session.Event) {
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

	if _, err := agent.Run(context.Background(), []AgentMessage{{Role: "user", Content: "initial"}}); err != nil {
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
		if message.Role == llm.RoleUser && message.Content == "queued follow-up" {
			sawFollowUpCommit = true
			break
		}
	}
	if !sawFollowUpCommit {
		t.Fatalf("committed = %#v, want queued follow-up model message", committed)
	}
}

func TestAgentStreamErrorDoesNotCommitAssistantMessage(t *testing.T) {
	var events []session.Event
	var committed []llm.Message
	streamErr := errors.New("provider stream failed")
	agent := New(AgentLoopConfig{
		Model: llm.Model{ID: "model"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{
				chunks: []*llm.Chunk{{Content: "partial response"}},
				err:    streamErr,
			}, nil
		},
		OnEvent: func(ev session.Event) {
			events = append(events, ev)
		},
		OnModelMessage: func(ctx context.Context, message llm.Message) error {
			committed = append(committed, message)
			return nil
		},
	})

	messages, err := agent.Run(context.Background(), []AgentMessage{{
		Role:    "user",
		Content: "hello",
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
		if _, ok := ev.(session.TurnFinished); ok {
			t.Fatalf("unexpected turn-finished event after stream error: %#v", ev)
		}
	}
}

func TestSessionAdapterResumeHydratesModelHistory(t *testing.T) {
	store, err := storage.NewEphemeralCantoStore()
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

	adapter := NewSessionAdapter(&SessionAdapterConfig{
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
	messages := adapter.agent.State().Messages
	if len(messages) != 2 {
		t.Fatalf("messages = %#v", messages)
	}
	if messages[0].Content != "prior" || messages[1].Content != "answer" {
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
