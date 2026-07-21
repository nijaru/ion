package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// INTEGRATION: Multi-turn conversation — two prompts produce two assistant
// responses, both are persisted.
func TestHarnessIntegration_MultiTurn(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	callCount := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callCount++
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "response " + string(rune('0'+callCount)), StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	// Turn 1
	msg1, err := h.Prompt(context.Background(), "first")
	if err != nil {
		t.Fatal(err)
	}
	am1 := msg1.(*session.AssistantMessage)
	if textContentMsg(am1) != "response 1" {
		t.Errorf("turn 1: expected 'response 1', got %q", textContentMsg(am1))
	}

	// Turn 2
	msg2, err := h.Prompt(context.Background(), "second")
	if err != nil {
		t.Fatal(err)
	}
	am2 := msg2.(*session.AssistantMessage)
	if textContentMsg(am2) != "response 2" {
		t.Errorf("turn 2: expected 'response 2', got %q", textContentMsg(am2))
	}

	// Both turns persisted.
	ctx := context.Background()
	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Messages) < 4 {
		t.Fatalf("expected at least 4 messages (user+asst * 2), got %d", len(snap.Messages))
	}
}

func TestHarnessIntegration_DurableTurnCommitAndReplay(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "ion.db")
	store, err := session.NewSQLiteStore(path, "durable-harness")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 0)
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "durable", StopReason: "stop"}}}, nil
		},
	})
	if _, err := h.Prompt(ctx, "persist durably"); err != nil {
		t.Fatal(err)
	}
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := session.NewSQLiteStore(path, "ignored")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	replayed := session.NewSession(reopened, 0)
	snapshot, err := replayed.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snapshot.Messages) != 2 || session.MessageText(snapshot.Messages[0]) != "persist durably" || session.MessageText(snapshot.Messages[1]) != "durable" {
		t.Fatalf("replayed durable messages = %#v, want user and assistant", snapshot.Messages)
	}
	interrupted, err := reopened.InterruptedTurns(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(interrupted) != 0 {
		t.Fatalf("interrupted turns after committed run = %+v", interrupted)
	}
}

func TestHarnessIntegration_DurableCancelledTurnDoesNotReplay(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "ion.db")
	store, err := session.NewSQLiteStore(path, "durable-cancel")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 0)
	started := make(chan struct{})
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			close(started)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	})
	done := make(chan struct{})
	go func() {
		_, _ = h.Prompt(ctx, "do not replay")
		close(done)
	}()
	<-started
	h.SetModel(llm.Model{ID: "must-not-commit"})
	if _, _, err := h.Abort(); err != nil {
		t.Fatal(err)
	}
	<-done
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := session.NewSQLiteStore(path, "ignored")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	replayed := session.NewSession(reopened, 0)
	snapshot, err := replayed.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snapshot.Messages) != 0 {
		t.Fatalf("cancelled durable messages replayed = %#v, want none", snapshot.Messages)
	}
	if snapshot.ActiveModel != "" {
		t.Fatalf("cancelled durable model change replayed = %q, want none", snapshot.ActiveModel)
	}
	if interrupted, err := reopened.InterruptedTurns(ctx); err != nil {
		t.Fatal(err)
	} else if len(interrupted) != 0 {
		t.Fatalf("explicitly aborted turns reported interrupted = %+v", interrupted)
	}
}

// INTEGRATION: Streaming — multi-chunk stream with interleaved content
// and reasoning accumulates correctly.
func TestHarnessIntegration_Streaming(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Reasoning: "think"},

			{Content: "Hello"},
			{Content: ", "},
			{Content: "world"},

			{Reasoning: "more"},
			{Reasoning: " thinking"},

			{Content: "!", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	msg, err := h.Prompt(context.Background(), "hi")
	if err != nil {
		t.Fatal(err)
	}
	am := msg.(*session.AssistantMessage)
	if textContentMsg(am) != "Hello, world!" {
		t.Errorf("expected 'Hello, world!', got %q", textContentMsg(am))
	}
	// Reasoning accumulated correctly (as ThinkingContent blocks).
	reasoning := reasoningContent(am)
	if reasoning != "thinkmore thinking" {
		t.Errorf("expected reasoning 'more thinking', got %q", reasoning)
	}
	if am.StopReason != session.StopReasonEndTurn {
		t.Errorf("expected end_turn, got %q", am.StopReason)
	}
}

// INTEGRATION: Tool calling — stream returns a tool call, harness
// executes the tool, tool result is persisted.
func TestHarnessIntegration_ToolCalling(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	// A mock tool that echoes its input.
	echoTool := Tool{
		Name:        "echo",
		Description: "Echoes input",
		Parameters:  `{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}`,
		Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{
				ToolCallID: id,
				ToolName:   "echo",
				Content:    []session.Content{session.TextContent{Text: "echo: " + string(args)}},
				Terminate:  true, // stop tool loop after execution
			}, nil
		},
	}

	callNum := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callNum++
		if callNum == 1 {
			// First call: tool call request.
			return &mockStream{chunks: []*llm.Chunk{
				{Calls: []llm.Call{{
					ID:   "call_1",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "echo", Arguments: `{"text":"hello"}`},
				}}},
				{StopReason: "stop"},
			}}, nil
		}
		// Second call: final response after tool execution.
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "done", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools:    []Tool{echoTool},
	})
	defer h.Close()

	msg, err := h.Prompt(context.Background(), "echo hello")
	if err != nil {
		t.Fatal(err)
	}
	_ = msg

	// Verify tool result was persisted.
	ctx := context.Background()
	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}

	found := false
	for _, m := range snap.Messages {
		if tr, ok := m.(*session.ToolResultMessage); ok {
			if tr.ToolName == "echo" {
				found = true
				break
			}
		}
	}
	if !found {
		msgs := make([]string, len(snap.Messages))
		for i, m := range snap.Messages {
			msgs[i] = msgTypeName(m)
		}
		t.Fatalf("expected ToolResultMessage for 'echo' in session, got: %v", msgs)
	}
}

// INTEGRATION: Persistence roundtrip — run a prompt, close the harness,
// create a new harness from the same store, verify messages are recovered.
func TestHarnessIntegration_PersistenceRoundtrip(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	// Run a prompt, but don't close the store yet.
	func() {
		streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "persisted response", StopReason: "stop"},
			}}, nil
		}
		h := NewController(ControllerConfig{
			Session:  sess,
			Model:    llm.Model{ID: "test"},
			StreamFn: streamFn,
			Store:    store,
		})

		_, err := h.Prompt(context.Background(), "remember me")
		if err != nil {
			t.Fatal(err)
		}
		// Don't close harness — it closes the store.
		// Just wait for idle and don't use the harness again.
	}()

	// Reopen session from the same store.
	sess2 := session.NewSession(store, 64)

	snap, err := sess2.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Messages) < 2 {
		t.Fatalf("expected at least 2 messages after reload, got %d", len(snap.Messages))
	}

	// Verify the response text survived.
	found := false
	for _, m := range snap.Messages {
		if am, ok := m.(*session.AssistantMessage); ok {
			if textContentMsg(am) == "persisted response" {
				found = true
			}
		}
	}
	if !found {
		t.Fatal("expected 'persisted response' in reloaded session")
	}
}

// INTEGRATION: Error recovery — stream failure returns error, harness
// returns to idle and can accept a new prompt.
func TestHarnessIntegration_ErrorRecovery(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	injectedErr := errors.New("provider exploded")
	callNum := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callNum++
		if callNum == 1 {
			return nil, injectedErr
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "recovered", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	// Turn 1 fails.
	msg1, err := h.Prompt(context.Background(), "first")
	var turnErr *TurnError
	if !errors.As(err, &turnErr) || turnErr.Kind != KindTool || turnErr.Recovery != RecoveryAbort {
		t.Fatalf("turn 1 error = %v, want failed TurnError", err)
	}
	if msg1 == nil {
		t.Fatal("turn 1 did not return the terminal assistant message")
	}
	am1 := msg1.(*session.AssistantMessage)
	if am1.StopReason != session.StopReasonError {
		t.Errorf("turn 1: expected error stop reason, got %q", am1.StopReason)
	}

	// Turn 2 succeeds — harness must be back to idle.
	msg2, err := h.Prompt(context.Background(), "second")
	if err != nil {
		t.Fatal(err)
	}
	am2 := msg2.(*session.AssistantMessage)
	if am2.StopReason != session.StopReasonEndTurn {
		t.Errorf("turn 2: expected end_turn, got %q", am2.StopReason)
	}
	if textContentMsg(am2) != "recovered" {
		t.Errorf("turn 2: expected 'recovered', got %q", textContentMsg(am2))
	}

	// Both turns persisted (error turn + normal turn).
	ctx := context.Background()
	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Messages) < 4 {
		t.Fatalf("expected at least 4 messages, got %d", len(snap.Messages))
	}
}

// INTEGRATION: Cancellation — cancel a run mid-stream via Abort(), verify
// harness returns to idle and can accept new prompts.
func TestHarnessIntegration_Cancel(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	started := make(chan struct{})
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		close(started)
		// Block until cancelled.
		<-ctx.Done()
		return nil, ctx.Err()
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	// Launch prompt in background.
	hPromptDone := make(chan struct{})
	go func() {
		defer close(hPromptDone)
		h.Prompt(context.Background(), "cancel me")
	}()

	// Wait for stream to start, then abort.
	<-started
	h.Abort()

	// Controller must return to idle.
	<-hPromptDone

	// Verify harness can accept new prompt.
	streamFn2 := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "after cancel", StopReason: "stop"},
		}}, nil
	}

	h2 := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn2,
	})
	defer h2.Close()

	msg, err := h2.Prompt(context.Background(), "after")
	if err != nil {
		t.Fatal(err)
	}
	am := msg.(*session.AssistantMessage)
	if textContentMsg(am) != "after cancel" {
		t.Errorf("expected 'after cancel', got %q", textContentMsg(am))
	}
}

// INTEGRATION: Concurrent safe — multiple event subscriber goroutines
// do not race.
func TestHarnessIntegration_ConcurrentSubscribers(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "safe", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})

	// Register 5 concurrent subscribers.
	var wg sync.WaitGroup
	subs := make([]*EventSubscription, 0, 5)
	for range 5 {
		sub, err := h.Subscribe(context.Background(), EventCursor{})
		if err != nil {
			t.Fatal(err)
		}
		subs = append(subs, sub)
		wg.Add(1)
		go func(sub *EventSubscription) {
			defer wg.Done()
			for range sub.Events {
				// drain
			}
		}(sub)
	}

	// Run a prompt and wait for completion before closing.
	_, err := h.Prompt(context.Background(), "concurrent")
	if err != nil {
		t.Fatal(err)
	}

	h.Close()
	wg.Wait()
}

// INTEGRATION: Context overflow recovery — when the model returns a
// context overflow error, the harness compacts and retries.
func TestHarnessIntegration_ContextOverflow(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	// Pre-fill session with messages to exceed the small context window.
	// Use a single harness for all pre-fill turns.
	func() {
		h := NewController(ControllerConfig{
			Session: sess,
			Model:   llm.Model{ID: "test"},
			StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
				return &mockStream{chunks: []*llm.Chunk{
					{Content: "filler", StopReason: "stop"},
				}}, nil
			},
		})
		for i := 0; i < 20; i++ {
			_, err := h.Prompt(context.Background(), "fill")
			if err != nil {
				t.Fatal(err)
			}
		}
	}()

	callNum := int32(0)
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		n := atomic.AddInt32(&callNum, 1)
		// First call: overflow. Subsequent calls: success.
		if n == 1 {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "context_length_exceeded: too many tokens", StopReason: "stop"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "recovered after compact", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:       sess,
		Model:         llm.Model{ID: "test"},
		StreamFn:      streamFn,
		Compaction:    CompactionSettings{Enabled: true, ReserveTokens: 10, KeepRecentTokens: 10},
		ContextWindow: 100,
	})
	defer h.Close()

	msg, err := h.Prompt(context.Background(), "test overflow")
	if err != nil {
		t.Fatal(err)
	}
	am := msg.(*session.AssistantMessage)
	if textContentMsg(am) != "recovered after compact" {
		t.Errorf("expected 'recovered after compact', got %q", textContentMsg(am))
	}
}

// INTEGRATION: Steering — inject a message mid-turn via Steer().
// The steered message should appear in the next provider request.
func TestHarnessIntegration_Steering(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	// First call: tool call. After tool executes, steer a follow-up.
	callNum := 0
	var seenSteer bool
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callNum++
		// Check if steer message appears in the request.
		for _, m := range req.Messages {
			if m.Role == llm.RoleUser && strings.Contains(m.Content, "steered") {
				seenSteer = true
			}
		}
		if callNum == 1 {
			return &mockStream{chunks: []*llm.Chunk{
				{Calls: []llm.Call{{
					ID:   "call_steer",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "echo", Arguments: `{}`},
				}}},
				{StopReason: "stop"},
			}}, nil
		}
		// Second call: final response.
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "steered response", StopReason: "stop"},
		}}, nil
	}

	echoTool := Tool{
		Name:        "echo",
		Description: "echo",
		Parameters:  `{"type":"object","properties":{}}`,
		Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{
				ToolCallID: id,
				ToolName:   "echo",
				Content:    []session.Content{session.TextContent{Text: "ok"}},
				// Don't terminate — inner loop must continue to drain steer.
			}, nil
		},
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools:    []Tool{echoTool},
	})
	defer h.Close()

	// Start a turn but steer a message after the tool request is sent.
	// We subscribe to events and inject when we see the tool call start.
	sub, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer sub.Close()

	errCh := make(chan error, 1)
	msgCh := make(chan session.Message, 1)
	go func() {
		msg, err := h.Prompt(context.Background(), "use echo")
		errCh <- err
		msgCh <- msg
	}()

	// Watch for tool execution start, then steer.
	for ev := range sub.Events {
		if _, ok := ev.Event.(session.ToolExecStart); ok {
			h.Steer("steered message")
			break
		}
	}

	err = <-errCh
	if err != nil {
		t.Fatal(err)
	}
	_ = <-msgCh

	if !seenSteer {
		t.Error("steered message was not visible in provider request")
	}
}

// INTEGRATION: FollowUp — inject after assistant stops, triggers a new continuation.
func TestHarnessIntegration_FollowUp(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	callNum := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callNum++
		if callNum == 1 {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "first", StopReason: "stop"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "followed up", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	sub, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer sub.Close()

	errCh := make(chan error, 1)
	msgCh := make(chan session.Message, 1)
	go func() {
		msg, err := h.Prompt(context.Background(), "hello")
		errCh <- err
		msgCh <- msg
	}()

	// Watch for the assistant response, inject follow-up.
	for ev := range sub.Events {
		if me, ok := ev.Event.(session.MessageEnd); ok {
			if _, isAssistant := me.Message.(*session.AssistantMessage); isAssistant {
				h.FollowUp("please continue")
				break
			}
		}
	}

	err = <-errCh
	if err != nil {
		t.Fatal(err)
	}
	msg := <-msgCh

	am := msg.(*session.AssistantMessage)
	if !strings.Contains(textContentMsg(am), "followed up") {
		t.Errorf("expected follow-up response, got %q", textContentMsg(am))
	}
}

// INTEGRATION: Tool execution failure — tool returns error, error propagated to caller.
func TestHarnessIntegration_ToolFailure(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Calls: []llm.Call{{
				ID:   "call_err",
				Type: "function",
				Function: struct {
					Name      string `json:"name"`
					Arguments string `json:"arguments"`
				}{Name: "failing", Arguments: `{}`},
			}}},
			{StopReason: "stop"},
		}}, nil
	}

	failingTool := Tool{
		Name:        "failing",
		Description: "always fails",
		Parameters:  `{"type":"object","properties":{}}`,
		Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{}, errors.New("tool crashed")
		},
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools:    []Tool{failingTool},
	})
	defer h.Close()

	msg, err := h.Prompt(context.Background(), "use failing")
	if err != nil {
		t.Fatalf("unexpected Prompt error: %v", err)
	}
	_ = msg

	// Tool errors produce ToolResultMessages with IsError=true. They are persisted
	// to session, not returned from Prompt. Check via BuildContext.
	ctx := context.Background()
	snap, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}

	found := false
	for _, m := range snap.Messages {
		if tr, ok := m.(*session.ToolResultMessage); ok && tr.IsError {
			if strings.Contains(textOfFirst(tr.Content), "tool crashed") {
				found = true
				break
			}
		}
	}
	if !found {
		t.Error("tool error ToolResultMessage not found in session")
	}
}

// INTEGRATION: Multiple sequential tool calls in one turn.
func TestHarnessIntegration_SequentialTools(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	var toolCount int
	callNum := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callNum++
		switch callNum {
		case 1:
			return &mockStream{chunks: []*llm.Chunk{
				{Calls: []llm.Call{{
					ID:   "tool_a",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "counter", Arguments: `{}`},
				}}},
				{StopReason: "stop"},
			}}, nil
		case 2:
			return &mockStream{chunks: []*llm.Chunk{
				{Calls: []llm.Call{{
					ID:   "tool_b",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "counter", Arguments: `{}`},
				}}},
				{StopReason: "stop"},
			}}, nil
		default:
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "all done", StopReason: "stop"},
			}}, nil
		}
	}

	counterTool := Tool{
		Name:        "counter",
		Description: "increments",
		Parameters:  `{"type":"object","properties":{}}`,
		Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			toolCount++
			return session.ToolResultMessage{
				ToolCallID: id,
				ToolName:   "counter",
				Content:    []session.Content{session.TextContent{Text: "ok"}},
				// Don't terminate — we need the loop to continue for tool_b.
			}, nil
		},
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools:    []Tool{counterTool},
	})
	defer h.Close()

	_, err := h.Prompt(context.Background(), "count twice")
	if err != nil {
		t.Fatal(err)
	}

	if toolCount != 2 {
		t.Errorf("expected 2 tool calls, got %d", toolCount)
	}
}

// INTEGRATION: Session resume — close and reopen from same store, verify state survives.
func TestHarnessIntegration_SessionResume(t *testing.T) {
	// Use a temporary file, not :memory:, to test real persistence.
	tmpDir := t.TempDir()
	dbPath := tmpDir + "/session.db"

	store, err := session.NewSQLiteStore(dbPath, "resume-test")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 64)

	callNum := 0
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		callNum++
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "turn" + string(rune('0'+callNum)), StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})

	_, err = h.Prompt(context.Background(), "turn 1")
	if err != nil {
		t.Fatal(err)
	}

	// Close harness and session.
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	// Reopen from same DB.
	store2, err := session.NewSQLiteStore(dbPath, "resume-test")
	if err != nil {
		t.Fatal(err)
	}
	defer store2.Close()

	sess2 := session.NewSession(store2, 64)

	h2 := NewController(ControllerConfig{
		Session:  sess2,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h2.Close()

	msg, err := h2.Prompt(context.Background(), "turn 2")
	if err != nil {
		t.Fatal(err)
	}

	am := msg.(*session.AssistantMessage)
	if textContentMsg(am) != "turn2" {
		t.Errorf("expected 'turn2', got %q", textContentMsg(am))
	}
}

// helpers

func textOfFirst(content []session.Content) string {
	for _, c := range content {
		if tc, ok := c.(session.TextContent); ok {
			return tc.Text
		}
	}
	return ""
}

func textContentMsg(msg *session.AssistantMessage) string {
	var parts []string
	for _, c := range msg.Content {
		if tc, ok := c.(session.TextContent); ok {
			parts = append(parts, tc.Text)
		}
	}
	return strings.Join(parts, "")
}

func reasoningContent(msg *session.AssistantMessage) string {
	var parts []string
	for _, c := range msg.Content {
		if tc, ok := c.(session.ThinkingContent); ok {
			parts = append(parts, tc.Text)
		}
	}
	return strings.Join(parts, "")
}

func msgTypeName(m session.Message) string {
	switch m.(type) {
	case *session.UserMessage:
		return "user"
	case *session.AssistantMessage:
		return "assistant"
	case *session.ToolResultMessage:
		return "tool_result"
	default:
		return "unknown"
	}
}

func typeName(v interface{}) string {
	switch v.(type) {
	case *session.QueueUpdate:
		return "QueueUpdate"
	case session.QueueUpdate:
		return "QueueUpdate"
	case session.TurnStart:
		return "TurnStart"
	case session.TurnEnd:
		return "TurnEnd"
	case session.AgentEnd:
		return "AgentEnd"
	case session.MessageStart:
		return "MessageStart"
	case session.MessageEnd:
		return "MessageEnd"
	case *session.Error:
		return "Error"
	case session.ToolExecStart:
		return "ToolExecStart"
	case session.ToolExecEnd:
		return "ToolExecEnd"
	default:
		return fmt.Sprintf("%T", v)
	}
}

// INTEGRATION: QueueDrainUpdate — QueueUpdate is emitted when the loop drains queues.
// Pi: drainQueuedMessages emits queue update after draining (agent-harness.js:337).
func TestHarnessIntegration_QueueDrainUpdate(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	h := NewController(ControllerConfig{
		Session: sess,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "ok", StopReason: "stop"},
			}}, nil
		},
	})
	defer h.Close()

	// Use Subscribe() listener to reliably capture events.
	var events []string
	var mu sync.Mutex
	queueUpdate := make(chan struct{}, 1)
	unsub := watchEvents(t, h, func(e session.Event) {
		mu.Lock()
		defer mu.Unlock()
		events = append(events, typeName(e))
		if _, ok := e.(session.QueueUpdate); ok {
			select {
			case queueUpdate <- struct{}{}:
			default:
			}
		}
	})
	defer unsub()

	// Start turn.
	msg, err := h.Prompt(context.Background(), "hello")
	if err != nil {
		t.Fatal(err)
	}
	_ = msg
	select {
	case <-queueUpdate:
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for QueueUpdate event")
	}

	mu.Lock()
	t.Logf("events: %v", events)

	// At least one QueueUpdate should have been emitted.
	foundQueueUpdate := false
	for _, name := range events {
		if name == "QueueUpdate" {
			foundQueueUpdate = true
			break
		}
	}
	if !foundQueueUpdate {
		t.Errorf("no QueueUpdate in %d events: %v", len(events), events)
	}
	mu.Unlock()
}

// REGRESSION: overflow recovery retries the run, but the harness must emit
// exactly ONE terminal AgentEnd for the whole Prompt (DESIGN §1.3). Previously
// each RunLoop emitted its own AgentEnd, producing a double AgentEnd on retry.
func TestHarnessIntegration_SingleAgentEndOnOverflowRetry(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	callNum := int32(0)
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		n := atomic.AddInt32(&callNum, 1)
		// First call: overflow. Subsequent calls: success.
		if n == 1 {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "context_length_exceeded: too many tokens", StopReason: "stop"},
			}}, nil
		}
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "recovered after compact", StopReason: "stop"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:       sess,
		Model:         llm.Model{ID: "test"},
		StreamFn:      streamFn,
		Compaction:    CompactionSettings{Enabled: true, ReserveTokens: 10, KeepRecentTokens: 10},
		ContextWindow: 100,
	})
	defer h.Close()

	agentEnd := make(chan struct{})
	watchEvents(t, h, func(e session.Event) {
		if _, ok := e.(session.AgentEnd); ok {
			agentEnd <- struct{}{}
		}
	})

	if _, err := h.Prompt(context.Background(), "test overflow"); err != nil {
		t.Fatal(err)
	}
	select {
	case <-agentEnd:
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for AgentEnd event")
	}
}

// INTEGRATION: before_tool_call hook can block a tool. The harness must wire
// LoopConfig.BeforeToolCall to the HookBeforeToolCall extension point (previously
// dead — declared in the loop but never set in buildLoopConfig).
func TestHarnessIntegration_BeforeToolCallBlocks(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	executed := false
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Calls: []llm.Call{{
				ID: "tc1",
				Function: struct {
					Name      string `json:"name"`
					Arguments string `json:"arguments"`
				}{Name: "test-tool", Arguments: `{}`},
			}}, StopReason: "toolUse"},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools: []Tool{{
			Name: "test-tool",
			Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
				executed = true
				return session.ToolResultMessage{ToolCallID: id, ToolName: "test-tool", Content: []session.Content{session.TextContent{Text: "ran"}}, Timestamp: time.Now()}, nil
			},
		}},
	})
	defer h.Close()

	h.On(HookBeforeToolCall, func(payload any) (any, error) {
		return &ToolCallDecision{Block: true, Reason: "blocked by test"}, nil
	})

	blocked := make(chan session.ToolResultMessage, 1)
	watchEvents(t, h, func(e session.Event) {
		if te, ok := e.(session.ToolExecEnd); ok {
			blocked <- te.Result
		}
	})

	if _, err := h.Prompt(context.Background(), "use the tool"); err != nil {
		t.Fatal(err)
	}
	var blockedResult session.ToolResultMessage
	select {
	case blockedResult = <-blocked:
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for blocked tool result")
	}

	if executed {
		t.Fatal("tool must NOT execute when before_tool_call blocks")
	}
	if !blockedResult.IsError {
		t.Fatal("blocked tool result must be an error")
	}
	var got string
	for _, c := range blockedResult.Content {
		if tc, ok := c.(session.TextContent); ok {
			got += tc.Text
		}
	}
	if !strings.Contains(got, "blocked by test") {
		t.Fatalf("expected block reason in tool result, got %q", got)
	}
}

func extractText(c session.ToolResultMessage) string {
	var s string
	for _, ct := range c.Content {
		if tc, ok := ct.(session.TextContent); ok {
			s += tc.Text
		}
	}
	return s
}

// INTEGRATION: sequential tool preparation. When multiple tools are called in
// one assistant message, the harness must prepare them sequentially (find,
// validate, before_tool_call hook) before executing concurrently. This test
// sends two tools and blocks one via the hook — the other must still execute.
func TestHarnessIntegration_SequentialPrep_MixedBlockAllow(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	var toolARan atomic.Bool
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{
				Calls: []llm.Call{
					{ID: "tc-block", Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "block-me", Arguments: `{}`}},
					{ID: "tc-run", Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "run-me", Arguments: `{}`}},
				},
				StopReason: "toolUse",
			},
		}}, nil
	}

	h := NewController(ControllerConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools: []Tool{
			{
				Name: "block-me",
				Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
					return session.ToolResultMessage{ToolCallID: id, ToolName: "block-me", Content: []session.Content{session.TextContent{Text: "should not run"}}, Timestamp: time.Now()}, nil
				},
			},
			{
				Name: "run-me",
				Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
					toolARan.Store(true)
					return session.ToolResultMessage{ToolCallID: id, ToolName: "run-me", Content: []session.Content{session.TextContent{Text: "A ran ok"}}, Timestamp: time.Now()}, nil
				},
			},
		},
	})
	defer h.Close()

	// Hook blocks "block-me" but allows "run-me".
	h.On(HookBeforeToolCall, func(payload any) (any, error) {
		btc := payload.(beforeToolCallPayload)
		if btc.ToolName == "block-me" {
			return &ToolCallDecision{Block: true, Reason: "blocked by test"}, nil
		}
		return nil, nil
	})

	var blockResult, runResult session.ToolResultMessage
	var mu sync.Mutex
	watchEvents(t, h, func(e session.Event) {
		if te, ok := e.(session.ToolExecEnd); ok {
			mu.Lock()
			defer mu.Unlock()
			switch te.ToolCallID {
			case "tc-block":
				blockResult = te.Result
			case "tc-run":
				runResult = te.Result
			}
		}
	})

	_, err := h.Prompt(context.Background(), "use both tools")
	if err != nil {
		t.Fatal(err)
	}

	mu.Lock()
	defer mu.Unlock()

	if !blockResult.IsError {
		t.Fatal("block-me must be blocked")
	}
	if !strings.Contains(extractText(blockResult), "blocked by test") {
		t.Fatalf("expected block reason, got %q", extractText(blockResult))
	}
	if !toolARan.Load() {
		t.Fatal("run-me must execute even though block-me was blocked")
	}
	if runResult.IsError {
		t.Fatalf("run-me must succeed, got error: %s", extractText(runResult))
	}
}
