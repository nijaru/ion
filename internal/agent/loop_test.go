package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
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
func (s *mockStream) Err() error   { return nil }
func (s *mockStream) Close() error { return nil }

type faultStream struct {
	chunk      *llm.Chunk
	ok         bool
	emitted    bool
	streamErr  error
	closeErr   error
	closeCalls int
}

func (s *faultStream) Next() (*llm.Chunk, bool) {
	if s.emitted {
		return nil, false
	}
	s.emitted = true
	return s.chunk, s.ok
}
func (s *faultStream) Err() error { return s.streamErr }
func (s *faultStream) Close() error {
	s.closeCalls++
	return s.closeErr
}

type abortingStream struct {
	signal    chan struct{}
	closeErr  error
	emitted   bool
	closeOnce bool
}

func (s *abortingStream) Next() (*llm.Chunk, bool) {
	if s.emitted {
		return nil, false
	}
	s.emitted = true
	if !s.closeOnce {
		close(s.signal)
		s.closeOnce = true
	}
	return &llm.Chunk{Content: "partial"}, true
}
func (s *abortingStream) Err() error   { return nil }
func (s *abortingStream) Close() error { return s.closeErr }

// --- Loop contract tests ---

func TestIsContextOverflowUsesProviderClassifierBeforeFallback(t *testing.T) {
	providerErr := errors.New("provider-specific overflow")
	if !isContextOverflow(LoopConfig{ContextOverflow: func(err error) bool {
		return errors.Is(err, providerErr)
	}}, providerErr) {
		t.Fatal("provider classifier did not recognize overflow")
	}
	if !isContextOverflow(LoopConfig{}, errors.New("maximum context length exceeded")) {
		t.Fatal("fallback overflow classifier did not recognize overflow")
	}
}

func TestRunLoopContextTransformObservesRunCancellation(t *testing.T) {
	parent, cancel := context.WithTimeout(t.Context(), 200*time.Millisecond)
	defer cancel()
	signal := make(chan struct{})
	started := make(chan struct{})
	transformed := make(chan error, 1)

	go func() {
		<-started
		close(signal)
	}()

	runLoop(
		parent,
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			TransformCtx: func(ctx context.Context, msgs []session.Message) []session.Message {
				close(started)
				<-ctx.Done()
				transformed <- ctx.Err()
				return msgs
			},
			StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
				return nil, ctx.Err()
			},
		},
		func(session.Event) {},
		signal,
	)

	select {
	case err := <-transformed:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("transform context error = %v, want canceled", err)
		}
	case <-parent.Done():
		t.Fatalf("transform did not observe run cancellation before parent deadline: %v", parent.Err())
	}
}

func TestRunLoopPrepareNextTurnObservesRunCancellation(t *testing.T) {
	parent, cancel := context.WithTimeout(t.Context(), 200*time.Millisecond)
	defer cancel()
	signal := make(chan struct{})
	started := make(chan struct{})
	prepared := make(chan error, 1)

	go func() {
		<-started
		close(signal)
	}()

	runLoop(
		parent,
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			PrepareNextTurn: func(ctx context.Context, _ []session.ToolResultMessage) *NextTurnSnapshot {
				close(started)
				<-ctx.Done()
				prepared <- ctx.Err()
				return nil
			},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &mockStream{chunks: []*llm.Chunk{{Content: "done"}}}, nil
			},
		},
		func(session.Event) {},
		signal,
	)

	select {
	case err := <-prepared:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("prepare-next-turn context error = %v, want canceled", err)
		}
	case <-parent.Done():
		t.Fatalf("prepare-next-turn did not observe run cancellation before parent deadline: %v", parent.Err())
	}
}

func TestRunLoopContextOverflowRequiresReplaySafeFrontier(t *testing.T) {
	overflow := errors.New("provider-specific overflow")
	result := runLoop(
		context.Background(),
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:           llm.Model{ID: "test"},
			ContextOverflow: func(err error) bool { return errors.Is(err, overflow) },
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &faultStream{
					chunk:     &llm.Chunk{Content: "partial"},
					ok:        true,
					streamErr: overflow,
				}, nil
			},
		},
		func(session.Event) {},
		nil,
	)
	if result.replaySafeContextOverflow {
		t.Fatal("partial provider output made context overflow replayable")
	}
}

func TestRunLoopContextOverflowBeforeStreamIsReplaySafe(t *testing.T) {
	overflow := errors.New("provider-specific overflow")
	result := runLoop(
		context.Background(),
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:           llm.Model{ID: "test"},
			ContextOverflow: func(err error) bool { return errors.Is(err, overflow) },
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return nil, overflow
			},
		},
		func(session.Event) {},
		nil,
	)
	if !result.replaySafeContextOverflow {
		t.Fatal("pre-response context overflow was not replayable")
	}
}

func TestRunLoopContextOverflowWithCloseFailureIsNotReplayable(t *testing.T) {
	overflow := errors.New("provider-specific overflow")
	closeErr := errors.New("stream cleanup failed")
	result := runLoop(
		context.Background(),
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:           llm.Model{ID: "test"},
			ContextOverflow: func(err error) bool { return errors.Is(err, overflow) },
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &faultStream{streamErr: overflow, closeErr: closeErr}, nil
			},
		},
		func(session.Event) {},
		nil,
	)
	if result.replaySafeContextOverflow {
		t.Fatal("stream cleanup failure made context overflow replayable")
	}
}

func TestRunLoopReturnedStreamWithOverflowErrorIsCleanedUp(t *testing.T) {
	overflow := errors.New("provider-specific overflow")
	closeErr := errors.New("returned stream cleanup failed")
	returned := &faultStream{closeErr: closeErr}
	result := runLoop(
		context.Background(),
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:           llm.Model{ID: "test"},
			ContextOverflow: func(err error) bool { return errors.Is(err, overflow) },
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return returned, overflow
			},
		},
		func(session.Event) {},
		nil,
	)
	if result.replaySafeContextOverflow {
		t.Fatal("returned stream cleanup failure made context overflow replayable")
	}
	if returned.closeCalls != 1 {
		t.Fatalf("returned stream close calls = %d, want one", returned.closeCalls)
	}
}

func TestRunLoopNilChunkWithOverflowErrorIsNotReplayable(t *testing.T) {
	overflow := errors.New("provider-specific overflow")
	result := runLoop(
		context.Background(),
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:           llm.Model{ID: "test"},
			ContextOverflow: func(err error) bool { return errors.Is(err, overflow) },
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &faultStream{ok: true, streamErr: overflow}, nil
			},
		},
		func(session.Event) {},
		nil,
	)
	if result.replaySafeContextOverflow {
		t.Fatal("nil-chunk protocol failure made context overflow replayable")
	}
}

func TestRunLoopCompletedEmptyResponseBlocksLaterOverflowReplay(t *testing.T) {
	overflow := errors.New("provider-specific overflow")
	followUp := []session.Message{session.NewUserText("follow-up", time.Now())}
	calls := 0
	result := runLoop(
		context.Background(),
		[]session.Message{session.NewUserText("prompt", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:           llm.Model{ID: "test"},
			ContextOverflow: func(err error) bool { return errors.Is(err, overflow) },
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				calls++
				if calls == 1 {
					return &mockStream{}, nil
				}
				return nil, overflow
			},
			DrainFollowUp: func() []session.Message {
				if len(followUp) == 0 {
					return nil
				}
				messages := followUp
				followUp = nil
				return messages
			},
		},
		func(session.Event) {},
		nil,
	)
	if calls != 2 {
		t.Fatalf("provider calls = %d, want empty response plus follow-up overflow", calls)
	}
	if result.replaySafeContextOverflow {
		t.Fatal("later overflow replayed across a completed response and follow-up")
	}
}

// INVARIANT: RunLoop emits AgentStart, then AgentEnd (lifecycle bookends).
func TestRunLoopLifecycleEvents(t *testing.T) {
	var events []session.Event
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "hello", StopReason: "stop"},
		}}, nil
	}

	RunLoop(
		context.Background(),
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

	RunLoop(
		context.Background(),
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

	msgs := RunLoop(
		context.Background(),
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

	msgs := RunLoop(
		context.Background(),
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

func TestRunLoopToolIterationLimitIsTerminalFailure(t *testing.T) {
	var agentEnd session.AgentEnd
	calls := 0
	msgs := RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("keep working", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:             llm.Model{ID: "test"},
			MaxToolIterations: 1,
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				calls++
				call := llm.Call{ID: fmt.Sprintf("call-%d", calls), Type: "function"}
				call.Function.Name = "continue"
				call.Function.Arguments = `{}`
				return &mockStream{
					chunks: []*llm.Chunk{{Calls: []llm.Call{call}, StopReason: llm.StopReasonToolUse}},
				}, nil
			},
			Tools: []Tool{
				{
					Name: "continue",
					Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
						return session.ToolResultMessage{ToolCallID: "tool", ToolName: "continue"}, nil
					},
				},
			},
		},
		func(event session.Event) {
			if end, ok := event.(session.AgentEnd); ok {
				agentEnd = end
			}
		},
		nil,
	)
	if calls != 1 {
		t.Fatalf("provider calls = %d, want one before iteration limit", calls)
	}
	if len(msgs) == 0 {
		t.Fatal("RunLoop returned no messages")
	}
	failure, ok := msgs[len(msgs)-1].(*session.AssistantMessage)
	if !ok || failure.StopReason != session.StopReasonError {
		t.Fatalf("last message = %#v, want terminal error assistant", msgs[len(msgs)-1])
	}
	if !strings.Contains(failure.Error, "max tool iterations (1) exceeded") {
		t.Fatalf("failure error = %q, want iteration-limit detail", failure.Error)
	}
	if len(agentEnd.Messages) == 0 || agentEnd.Messages[len(agentEnd.Messages)-1] != failure {
		t.Fatalf("AgentEnd messages = %#v, want terminal failure included", agentEnd.Messages)
	}
}

func TestRunLoopCleanErrorStopReasonIsTerminalBeforeToolExecution(t *testing.T) {
	var executed bool
	var agentEndCount int
	streamFn := func(context.Context, *llm.Request) (llm.Stream, error) {
		call := llm.Call{ID: "failed-call", Type: "function"}
		call.Function.Name = "write"
		call.Function.Arguments = `{}`
		return &mockStream{chunks: []*llm.Chunk{{
			Calls:      []llm.Call{call},
			StopReason: llm.StopReasonError,
		}}}, nil
	}

	msgs := RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:    llm.Model{ID: "test"},
			StreamFn: streamFn,
			Tools: []Tool{
				{
					Name: "write",
					Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
						executed = true
						return session.ToolResultMessage{}, nil
					},
				},
			},
		},
		func(event session.Event) {
			if _, ok := event.(session.AgentEnd); ok {
				agentEndCount++
			}
		},
		nil,
	)

	if executed {
		t.Fatal("executed a tool from a failed provider response")
	}
	if agentEndCount != 1 {
		t.Fatalf("AgentEnd count = %d, want 1", agentEndCount)
	}
	assistant, ok := msgs[len(msgs)-1].(*session.AssistantMessage)
	if !ok || assistant.StopReason != session.StopReasonError {
		t.Fatalf("last message = %#v, want error assistant", msgs[len(msgs)-1])
	}
	if assistant.Error == "" {
		t.Fatal("failed provider response did not retain an error message")
	}
}

func TestBuildAssistantMessageNormalizesFailureWithoutProviderDetail(t *testing.T) {
	for _, stopReason := range []llm.StopReason{llm.StopReasonError, llm.StopReasonAborted} {
		t.Run(string(stopReason), func(t *testing.T) {
			var acc llm.StreamAccumulator
			acc.Add(&llm.Chunk{StopReason: stopReason})

			message, err := buildAssistantMessage(acc, llm.Model{ID: "test"}, session.ThinkingOff)
			if err != nil {
				t.Fatal(err)
			}
			if message.StopReason != session.StopReason(stopReason) || message.Error == "" {
				t.Fatalf("message = %#v, want failure reason and error", message)
			}
		})
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

	RunLoop(
		context.Background(),
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

func TestRunLoopPreCancelledSignalSkipsProvider(t *testing.T) {
	called := false
	signal := make(chan struct{})
	close(signal)
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("cancel", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				called = true
				return nil, nil
			},
		},
		func(session.Event) {},
		signal,
	)
	if called {
		t.Fatal("provider called for pre-cancelled run")
	}
}

func TestRunLoopPreCancelledContextSkipsProvider(t *testing.T) {
	called := false
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	RunLoop(
		ctx,
		[]session.Message{session.NewUserText("cancel", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				called = true
				return nil, nil
			},
		},
		func(session.Event) {},
		nil,
	)
	if called {
		t.Fatal("provider called for pre-cancelled context")
	}
}

// INVARIANT: RunLoop is stateless — no fields, no persistence.
// This is a compile-time invariant enforced by rg guard, but we verify
// the function signature takes all inputs as args.
func TestDefaultConvertOmitsFailedAndEmptyAssistantRecords(t *testing.T) {
	messages := DefaultConvert([]session.Message{
		session.NewUserText("prompt", time.Now()),
		&session.AssistantMessage{
			Content:    []session.Content{session.TextContent{Text: "partial"}},
			StopReason: session.StopReasonError,
		},
		&session.AssistantMessage{
			Content:    []session.Content{session.ThinkingContent{Text: "incomplete"}},
			StopReason: session.StopReasonAborted,
		},
		&session.AssistantMessage{StopReason: session.StopReasonEndTurn},
	})
	if len(messages) != 1 || messages[0].Role != llm.RoleUser {
		t.Fatalf("converted messages = %#v, want only the user prompt", messages)
	}
}

func TestDefaultConvertPreservesThinkingMetadata(t *testing.T) {
	messages := DefaultConvert([]session.Message{&session.AssistantMessage{
		Content: []session.Content{session.ThinkingContent{
			Text: "reasoning", Signature: "sig", Redacted: true,
		}},
	}})
	if len(messages) != 1 || len(messages[0].ThinkingBlocks) != 1 {
		t.Fatalf("converted messages = %#v, want one thinking block", messages)
	}
	block := messages[0].ThinkingBlocks[0]
	if block.Thinking != "reasoning" || block.Signature != "sig" || !block.Redacted {
		t.Fatalf("thinking block = %#v, want metadata preserved", block)
	}
}

func TestDefaultConvertPreservesToolResultError(t *testing.T) {
	messages := DefaultConvert([]session.Message{&session.ToolResultMessage{
		ToolCallID: "call-1",
		ToolName:   "read",
		Content:    []session.Content{session.TextContent{Text: "failed"}},
		IsError:    true,
	}})
	if len(messages) != 1 || !messages[0].IsError || messages[0].ToolID != "call-1" {
		t.Fatalf("converted tool result = %#v, want error result", messages)
	}
}

func TestRunLoopIsStateless(t *testing.T) {
	// Verify RunLoop can be called with no prior state.
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{StopReason: "stop"}}}, nil
	}
	// Each call is independent — no shared state.
	RunLoop(
		context.Background(),
		nil,
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "x"}, StreamFn: streamFn},
		func(e session.Event) {},
		nil,
	)
	RunLoop(
		context.Background(),
		nil,
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "y"}, StreamFn: streamFn},
		func(e session.Event) {},
		nil,
	)
}

// INVARIANT: tool calls are executed and ToolExecStart/End emitted.
func TestRunLoopToolExecution(t *testing.T) {
	var toolEvents []session.Event
	var updateEvents []session.MessageUpdate
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

	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("use tool", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}, StreamFn: streamFn, Tools: []Tool{tool}},
		func(e session.Event) {
			toolEvents = append(toolEvents, e)
			if update, ok := e.(session.MessageUpdate); ok {
				updateEvents = append(updateEvents, update)
			}
		},
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
	foundRawArguments := false
	for _, update := range updateEvents {
		if delta, ok := update.Delta.(session.ToolCallDelta); ok {
			if delta.ArgumentsChunk != `{"x":1}` {
				t.Fatalf("tool arguments chunk = %q, want raw JSON", delta.ArgumentsChunk)
			}
			foundRawArguments = true
		}
	}
	if !foundRawArguments {
		t.Fatal("expected tool call message update")
	}
}

func TestRunLoopProjectsStructuredBlocksAndRawToolDeltas(t *testing.T) {
	var updates []session.MessageUpdate
	callCount := 0
	streamFn := func(context.Context, *llm.Request) (llm.Stream, error) {
		callCount++
		if callCount == 1 {
			return &mockStream{chunks: []*llm.Chunk{
				{Block: llm.ToolCallBlock{
					ID: "call-structured", Name: "structured-tool", Arguments: `{"path":"`, Type: "function",
				}},
				{Block: llm.ToolCallBlock{
					ID: "call-structured", Name: "structured-tool", Arguments: `{"path":"file.go"}`, Type: "function",
				}, StopReason: "toolUse"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Block: llm.TextBlock{Text: "structured response"}, StopReason: "stop"},
		}}, nil
	}

	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("use structured tool", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:    llm.Model{ID: "test"},
			StreamFn: streamFn,
			Tools: []Tool{
				{
					Name: "structured-tool",
					Execute: func(_ context.Context, id string, args json.RawMessage, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
						return session.ToolResultMessage{
							ToolCallID: id,
							ToolName:   "structured-tool",
							Content:    []session.Content{session.TextContent{Text: string(args)}},
							Timestamp:  time.Now(),
						}, nil
					},
				},
			},
		},
		func(event session.Event) {
			if update, ok := event.(session.MessageUpdate); ok {
				updates = append(updates, update)
			}
		},
		nil,
	)

	var argumentDeltas []string
	var textDelta string
	for _, update := range updates {
		switch delta := update.Delta.(type) {
		case session.ToolCallDelta:
			argumentDeltas = append(argumentDeltas, delta.ArgumentsChunk)
		case session.TextDelta:
			textDelta += delta.Text
		}
	}
	if len(argumentDeltas) != 2 || argumentDeltas[0] != `{"path":"` || argumentDeltas[1] != `file.go"}` {
		t.Fatalf("tool argument deltas = %#v, want raw incremental chunks", argumentDeltas)
	}
	if textDelta != "structured response" {
		t.Fatalf("structured text delta = %q, want response text", textDelta)
	}
}

func TestPrepareToolCallRecoversBeforeHookPanic(t *testing.T) {
	tool := Tool{Name: "hook-panic"}
	_, result := prepareToolCall(context.Background(), TurnContext{}, session.AssistantMessage{}, &session.ToolCall{
		ID: "call-hook-panic", Name: "hook-panic", Arguments: map[string]any{},
	}, LoopConfig{
		Tools: []Tool{tool},
		BeforeToolCall: func(ToolCallContext) *ToolCallDecision {
			panic("bad hook")
		},
	}, nil)
	if result == nil || !result.IsError {
		t.Fatalf("result = %#v, want recovered hook error", result)
	}
}

func TestPrepareToolArgumentsRejectsTrailingJSON(t *testing.T) {
	tool := &Tool{
		Name: "trailing-prepare",
		PrepareArgs: func(json.RawMessage) json.RawMessage {
			return json.RawMessage(`{"ok":true} {"ignored":true}`)
		},
	}
	_, err := prepareToolArguments(tool, map[string]any{})
	if err == nil {
		t.Fatal("trailing prepared JSON accepted")
	}
}

func TestPrepareToolArgumentsRecoversPanics(t *testing.T) {
	tool := &Tool{
		Name: "panic-prepare",
		PrepareArgs: func(json.RawMessage) json.RawMessage {
			panic("bad preparation")
		},
	}
	prepared, result := prepareToolCall(
		context.Background(),
		TurnContext{},
		session.AssistantMessage{},
		&session.ToolCall{
			ID: "call-panic", Name: "panic-prepare", Arguments: map[string]any{},
		},
		LoopConfig{Tools: []Tool{*tool}},
		nil,
	)
	if prepared.tool != nil || result == nil || !result.IsError {
		t.Fatalf("prepared=%#v result=%#v, want recovered preparation error", prepared, result)
	}
}

func TestParallelPreparationHonorsPreCancelledSignalWithoutHook(t *testing.T) {
	called := false
	signal := make(chan struct{})
	close(signal)
	tool := Tool{
		Name: "cancelled-tool",
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			called = true
			return session.ToolResultMessage{}, nil
		},
	}
	results, _ := executeToolCallsParallel(
		context.Background(), TurnContext{}, session.AssistantMessage{},
		[]*session.ToolCall{
			{ID: "call-cancelled", Name: "cancelled-tool", Arguments: map[string]any{}},
			{ID: "call-skipped", Name: "cancelled-tool", Arguments: map[string]any{}},
		},
		LoopConfig{Tools: []Tool{tool}}, func(session.Event) {}, signal,
	)
	if called {
		t.Fatal("cancelled parallel tool executed")
	}
	if len(results) != 1 || !results[0].IsError || results[0].ToolCallID != "call-cancelled" {
		t.Fatalf("results = %#v, want one finalized cancellation error", results)
	}
}

func TestParallelInvalidToolCallsHaveStartBeforeEnd(t *testing.T) {
	var events []session.Event
	tool := Tool{
		Name:       "required-tool",
		Parameters: `{"type":"object","required":["path"]}`,
	}
	calls := []*session.ToolCall{
		{ID: "call-a", Name: "required-tool", Arguments: map[string]any{}},
		{ID: "call-b", Name: "missing", Arguments: map[string]any{}},
	}
	results, _ := executeToolCallsParallel(
		context.Background(),
		TurnContext{},
		session.AssistantMessage{},
		calls,
		LoopConfig{Tools: []Tool{tool}},
		func(event session.Event) { events = append(events, event) },
		nil,
	)
	if len(results) != len(calls) {
		t.Fatalf("results = %d, want %d", len(results), len(calls))
	}
	for _, call := range calls {
		start, end := -1, -1
		for i, event := range events {
			switch event := event.(type) {
			case session.ToolExecStart:
				if event.ToolCallID == call.ID {
					start = i
				}
			case session.ToolExecEnd:
				if event.ToolCallID == call.ID {
					end = i
				}
			}
		}
		if start < 0 || end < 0 || start >= end {
			t.Fatalf("events for %s = %#v, want start before end", call.ID, events)
		}
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
		Tools: []Tool{
			{
				Name: "panic_tool",
				Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
					panic("intentional test panic")
				},
			},
		},
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

// INVARIANT: AfterToolCall hook panics produce error tool results, matching Pi's finalizeExecutedToolCall try/catch.
func TestRunLoopAfterToolCallPanicRecovery(t *testing.T) {
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
					}{Name: "echo", Arguments: `{}`},
				}}, StopReason: "toolUse"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "after panic", StopReason: "stop"},
		}}, nil
	}

	cfg := LoopConfig{
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools: []Tool{
			{
				Name: "echo",
				Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
					return session.ToolResultMessage{
						ToolCallID: id,
						ToolName:   "echo",
						Content:    []session.Content{session.TextContent{Text: "ok"}},
					}, nil
				},
			},
		},
		AfterToolCall: func(ctx ToolCallResultContext) *ToolCallPatch {
			panic("hook bug")
		},
		Convert: DefaultConvert,
	}

	msgs := RunLoop(context.Background(), nil, TurnContext{}, cfg, func(e session.Event) {}, nil)

	// The tool result should be marked as error from the AfterToolCall panic.
	foundHookError := false
	for _, m := range msgs {
		switch m := m.(type) {
		case *session.ToolResultMessage:
			if m.ToolName == "echo" && m.IsError {
				for _, c := range m.Content {
					if tc, ok := c.(session.TextContent); ok {
						if strings.Contains(tc.Text, "hook panic") {
							foundHookError = true
						}
					}
				}
			}
		}
	}

	if !foundHookError {
		t.Fatal("expected AfterToolCall panic to produce error tool result")
	}

	// The turn should NOT crash — the assistant should continue with a second LLM call.
	if callCount < 2 {
		t.Fatal("turn should continue after AfterToolCall panic (expected >=2 LLM calls)")
	}
}

// INVARIANT: shouldTerminateToolBatch returns true only when every result has Terminate=true.
func TestShouldTerminateToolBatch(t *testing.T) {
	// Empty batch.
	if shouldTerminateToolBatch(nil) {
		t.Fatal("empty batch must not terminate")
	}
	// Single non-terminating result.
	if shouldTerminateToolBatch([]session.ToolResultMessage{{Terminate: false}}) {
		t.Fatal("single non-terminate must not terminate")
	}
	// Single terminating result.
	if !shouldTerminateToolBatch([]session.ToolResultMessage{{Terminate: true}}) {
		t.Fatal("single terminate must terminate")
	}
	// Mixed — one false means no termination.
	if shouldTerminateToolBatch([]session.ToolResultMessage{{Terminate: true}, {Terminate: false}}) {
		t.Fatal("mixed batch must not terminate")
	}
	// All terminating.
	if !shouldTerminateToolBatch([]session.ToolResultMessage{{Terminate: true}, {Terminate: true}, {Terminate: true}}) {
		t.Fatal("all-terminate batch must terminate")
	}
}

// INVARIANT: tool result messages emit MessageStart+MessageEnd events.
func TestRunLoopToolResultMessageEvents(t *testing.T) {
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
					}{Name: "test-tool", Arguments: `{"x":1}`},
				}}, StopReason: "toolUse"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "done", StopReason: "stop"},
		}}, nil
	}

	var msEvents int
	vars := RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:    llm.Model{ID: "test"},
			StreamFn: streamFn,
			Tools: []Tool{
				{
					Name: "test-tool",
					Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
						return session.ToolResultMessage{
							ToolCallID: id,
							ToolName:   "test-tool",
							Content:    []session.Content{session.TextContent{Text: "ok"}},
							Timestamp:  time.Now(),
						}, nil
					},
				},
			},
		},
		func(e session.Event) {
			switch e.(type) {
			case session.MessageStart:
				msEvents++
			case session.MessageEnd:
				msEvents++
			}
		},
		nil,
	)
	_ = vars

	// Expect: prompt MessageStart+End(2) + assistant MessageStart+End(2) + tool-result MessageStart+End(2) = 6.
	if msEvents < 6 {
		t.Fatalf("expected at least 6 MessageStart/End events (prompt, assistant, tool-result), got %d", msEvents)
	}
}

// REGRESSION: when the response is truncated by the output token limit
// (stopReason "length"), streamed tool-call arguments are garbage, so the
// loop must NOT execute them — it emits error tool results instead (Pi's
// failToolCallsFromTruncatedMessage).
func TestRunLoopStopReasonLengthFailsToolCalls(t *testing.T) {
	executed := false
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{
				Calls: []llm.Call{{
					ID: "tc1",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "test-tool", Arguments: `{}`},
				}},
				StopReason: "length",
			},
		}}, nil
	}

	var toolEvents []session.Event
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model:    llm.Model{ID: "test"},
			StreamFn: streamFn,
			Tools: []Tool{
				{
					Name: "test-tool",
					Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
						executed = true
						return session.ToolResultMessage{
							ToolCallID: id,
							ToolName:   "test-tool",
							Content:    []session.Content{session.TextContent{Text: "ok"}},
						}, nil
					},
				},
			},
		},
		func(e session.Event) {
			switch e.(type) {
			case session.ToolExecStart, session.ToolExecEnd:
				toolEvents = append(toolEvents, e)
			}
		},
		nil,
	)

	if executed {
		t.Fatal("tool must NOT execute when stopReason is length")
	}
	if len(toolEvents) != 2 {
		t.Fatalf("expected 2 tool events (start + end error), got %d", len(toolEvents))
	}
	end, ok := toolEvents[1].(session.ToolExecEnd)
	if !ok {
		t.Fatalf("expected ToolExecEnd, got %T", toolEvents[1])
	}
	if !end.Result.IsError {
		t.Fatal("truncated tool result must be an error")
	}
}

// REGRESSION: a clean cancellation mid-stream (provider returns ok=false with a
// nil stream error) must be classified as an aborted turn, not a completed one.
// Previously the partial response was finalized and treated as success.
func TestRunLoopMissingStreamFunctionIsTerminalFailure(t *testing.T) {
	var turnEnd *session.TurnEnd
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}},
		func(event session.Event) {
			if message, ok := event.(session.TurnEnd); ok {
				turnEnd = &message
			}
		},
		nil,
	)
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || !strings.Contains(assistant.Error, "stream function is not configured") {
		t.Fatalf("turn-end message = %#v, want missing stream failure", turnEnd.Message)
	}
}

func TestRunLoopNilStreamIsTerminalFailure(t *testing.T) {
	var turnEnd *session.TurnEnd
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return nil, nil
			},
		},
		func(event session.Event) {
			if message, ok := event.(session.TurnEnd); ok {
				turnEnd = &message
			}
		},
		nil,
	)
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || !strings.Contains(assistant.Error, "provider returned a nil stream") {
		t.Fatalf("turn-end message = %#v, want nil-stream failure", turnEnd.Message)
	}
}

func TestRunLoopNilStreamChunkIsTerminalFailure(t *testing.T) {
	var turnEnd *session.TurnEnd
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &faultStream{ok: true}, nil
			},
		},
		func(event session.Event) {
			if message, ok := event.(session.TurnEnd); ok {
				turnEnd = &message
			}
		},
		nil,
	)
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || !strings.Contains(assistant.Error, "nil stream chunk") {
		t.Fatalf("turn-end message = %#v, want nil-chunk failure", turnEnd.Message)
	}
}

func TestRunLoopStreamErrorPreservesSafePartialContent(t *testing.T) {
	var turnEnd *session.TurnEnd
	streamErr := errors.New("stream read failed")
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &faultStream{
					chunk:     &llm.Chunk{Content: "partial"},
					ok:        true,
					streamErr: streamErr,
				}, nil
			},
		},
		func(event session.Event) {
			if message, ok := event.(session.TurnEnd); ok {
				turnEnd = &message
			}
		},
		nil,
	)
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || !strings.Contains(assistant.Error, streamErr.Error()) {
		t.Fatalf("turn-end message = %#v, want stream failure", turnEnd.Message)
	}
	if session.MessageText(assistant) != "partial" {
		t.Fatalf("partial failure text = %q, want preserved content", session.MessageText(assistant))
	}
}

func TestRunLoopStreamCloseFailureIsTerminalFailure(t *testing.T) {
	var turnEnd *session.TurnEnd
	closeErr := errors.New("stream close failed")
	stream := &faultStream{closeErr: closeErr}
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return stream, nil
			},
		},
		func(event session.Event) {
			if message, ok := event.(session.TurnEnd); ok {
				turnEnd = &message
			}
		},
		nil,
	)
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || !strings.Contains(assistant.Error, closeErr.Error()) {
		t.Fatalf("turn-end message = %#v, want close failure", turnEnd.Message)
	}
}

func TestRunLoopRejectsMalformedFinalToolArguments(t *testing.T) {
	var turnEnd *session.TurnEnd
	executed := false
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("use tool", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &mockStream{chunks: []*llm.Chunk{{
					Calls: []llm.Call{{
						ID: "malformed",
						Function: struct {
							Name      string `json:"name"`
							Arguments string `json:"arguments"`
						}{Name: "unsafe-tool", Arguments: `{"path":"file"} trailing`},
					}},
					StopReason: "toolUse",
				}}}, nil
			},
			Tools: []Tool{
				{
					Name: "unsafe-tool",
					Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
						executed = true
						return session.ToolResultMessage{}, nil
					},
				},
			},
		},
		func(event session.Event) {
			if message, ok := event.(session.TurnEnd); ok {
				turnEnd = &message
			}
		},
		nil,
	)
	if executed {
		t.Fatal("malformed tool arguments were executed")
	}
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || !strings.Contains(assistant.Error, "trailing JSON value") {
		t.Fatalf("turn-end message = %#v, want malformed-arguments failure", turnEnd.Message)
	}
}

func TestRunLoopAbortPreservesStreamCleanupFailure(t *testing.T) {
	signal := make(chan struct{})
	closeErr := errors.New("abort stream cleanup failed")
	var turnEnd *session.TurnEnd
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				return &abortingStream{signal: signal, closeErr: closeErr}, nil
			},
		},
		func(e session.Event) {
			if te, ok := e.(session.TurnEnd); ok {
				turnEnd = &te
			}
		},
		signal,
	)
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || !strings.Contains(assistant.Error, closeErr.Error()) {
		t.Fatalf("turn-end message = %#v, want cancellation cleanup failure", turnEnd.Message)
	}
}

func TestRunLoopAbortDuringStreamSetupClassifiedAsCancellation(t *testing.T) {
	signal := make(chan struct{})
	var turnEnd *session.TurnEnd
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{
			Model: llm.Model{ID: "test"},
			StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
				close(signal)
				return nil, &hookError{hookType: HookAfterProviderResponse, err: errors.New("hook blocked")}
			},
		},
		func(e session.Event) {
			if te, ok := e.(session.TurnEnd); ok {
				turnEnd = &te
			}
		},
		signal,
	)
	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	assistant, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok || assistant.StopReason != session.StopReasonAborted {
		t.Fatalf("turn-end message = %#v, want aborted assistant", turnEnd.Message)
	}
	if !strings.Contains(assistant.Error, "hook blocked") {
		t.Fatalf("turn-end error = %q, want setup failure evidence", assistant.Error)
	}
}

func TestRunLoopAbortMidStreamClassified(t *testing.T) {
	signal := make(chan struct{})
	close(signal) // aborted at stream start
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "partial", StopReason: "stop"},
		}}, nil
	}

	var turnEnd *session.TurnEnd
	RunLoop(
		context.Background(),
		[]session.Message{session.NewUserText("hi", time.Now())},
		TurnContext{},
		LoopConfig{Model: llm.Model{ID: "test"}, StreamFn: streamFn},
		func(e session.Event) {
			if te, ok := e.(session.TurnEnd); ok {
				turnEnd = &te
			}
		},
		signal,
	)

	if turnEnd == nil {
		t.Fatal("expected TurnEnd")
	}
	am, ok := turnEnd.Message.(*session.AssistantMessage)
	if !ok {
		t.Fatalf("expected AssistantMessage in TurnEnd, got %T", turnEnd.Message)
	}
	if am.StopReason != session.StopReasonAborted {
		t.Fatalf("expected aborted stop reason, got %q", am.StopReason)
	}
}
