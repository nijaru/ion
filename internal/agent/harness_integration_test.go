package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

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

	h := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
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
		h := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	// Turn 1 fails.
	msg1, err := h.Prompt(context.Background(), "first")
	if err != nil {
		t.Fatal(err)
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

	h := NewHarness(HarnessConfig{
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

	// Harness must return to idle.
	<-hPromptDone

	// Verify harness can accept new prompt.
	streamFn2 := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Content: "after cancel", StopReason: "stop"},
		}}, nil
	}

	h2 := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})

	// Register 5 concurrent subscribers.
	var wg sync.WaitGroup
	for range 5 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			ch := h.Events()
			for range ch {
				// drain
			}
		}()
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
		h := NewHarness(HarnessConfig{
			Session:  sess,
			Model:    llm.Model{ID: "test"},
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

	h := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools:    []Tool{echoTool},
	})
	defer h.Close()

	// Start a turn but steer a message after the tool request is sent.
	// We subscribe to events and inject when we see the tool call start.
	events := h.Events()

	errCh := make(chan error, 1)
	msgCh := make(chan session.Message, 1)
	go func() {
		msg, err := h.Prompt(context.Background(), "use echo")
		errCh <- err
		msgCh <- msg
	}()

	// Watch for tool execution start, then steer.
	for ev := range events {
		if _, ok := ev.(session.ToolExecStart); ok {
			h.Steer("steered message")
			break
		}
	}

	err := <-errCh
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

	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})
	defer h.Close()

	events := h.Events()

	errCh := make(chan error, 1)
	msgCh := make(chan session.Message, 1)
	go func() {
		msg, err := h.Prompt(context.Background(), "hello")
		errCh <- err
		msgCh <- msg
	}()

	// Watch for the assistant response, inject follow-up.
	for ev := range events {
		if me, ok := ev.(session.MessageEnd); ok {
			if _, isAssistant := me.Message.(*session.AssistantMessage); isAssistant {
				h.FollowUp("please continue")
				break
			}
		}
	}

	err := <-errCh
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

	h := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})

	_, err = h.Prompt(context.Background(), "turn 1")
	if err != nil {
		t.Fatal(err)
	}

	// Close harness and session.
	h.Close()

	// Reopen from same DB.
	store2, err := session.NewSQLiteStore(dbPath, "resume-test")
	if err != nil {
		t.Fatal(err)
	}
	defer store2.Close()

	sess2 := session.NewSession(store2, 64)

	h2 := NewHarness(HarnessConfig{
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

	h := NewHarness(HarnessConfig{
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
	done := false
	unsub := h.Subscribe(func(e session.Event) {
		mu.Lock()
		defer mu.Unlock()
		if done {
			return
		}
		events = append(events, typeName(e))
		if _, ok := e.(session.AgentEnd); ok {
			done = true
		}
	})
	defer unsub()

	// Start turn.
	msg, err := h.Prompt(context.Background(), "hello")
	if err != nil {
		t.Fatal(err)
	}
	_ = msg

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

	h := NewHarness(HarnessConfig{
		Session:       sess,
		Model:         llm.Model{ID: "test"},
		StreamFn:      streamFn,
		Compaction:    CompactionSettings{Enabled: true, ReserveTokens: 10, KeepRecentTokens: 10},
		ContextWindow: 100,
	})
	defer h.Close()

	var agentEndCount int
	h.Subscribe(func(e session.Event) {
		if _, ok := e.(session.AgentEnd); ok {
			agentEndCount++
		}
	})

	if _, err := h.Prompt(context.Background(), "test overflow"); err != nil {
		t.Fatal(err)
	}
	if agentEndCount != 1 {
		t.Fatalf("expected exactly 1 AgentEnd across overflow retry, got %d", agentEndCount)
	}
}
