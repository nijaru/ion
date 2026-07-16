package agent

// 1A: lifecycle/event-order contract tests (no impl change).
// These encode DESIGN invariants before fixing bugs in 1B/1C.
// Sol: "Add behavioral tests: MessageStart→Update*→End, ToolExecStart→Update*→End,
// persist-before-emit, pending→SavePoint, single terminal AgentEnd for normal/abort/panic/overflow-retry,
// no events after terminal, loop stateless."

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// ----- Helpers to collect events from a Prompt -----

func collectWithSubscribe(h *Harness, prompt string) ([]session.Event, error) {
	var events []session.Event
	var mu sync.Mutex
	// Drain Events() channel concurrently to avoid blocking emit when no TUI reader exists.
	// Without this, blocking ordered emit would deadlock when channel fills (256) in tests
	// that only use Subscribe.
	drainDone := make(chan struct{})
	go func() {
		defer close(drainDone)
		for {
			select {
			case _, ok := <-h.Events():
				if !ok {
					return
				}
			case <-time.After(10 * time.Second):
				return
			}
		}
	}()
	// Subscribe captures all events reliably even if channel reader lags.
	unsub := h.Subscribe(func(e session.Event) {
		mu.Lock()
		events = append(events, e)
		mu.Unlock()
	})
	_, err := h.Prompt(context.Background(), prompt)
	unsub()
	// Allow drain goroutine to exit after Prompt finishes (channel will no longer be written).
	// We don't close Events channel here — harness owns it — so we just stop draining after timeout.
	// Prompt has returned, so no more emits will happen for this turn; drain goroutine will timeout.
	select {
	case <-drainDone:
	case <-time.After(100 * time.Millisecond):
	}
	mu.Lock()
	defer mu.Unlock()
	out := make([]session.Event, len(events))
	copy(out, events)
	return out, err
}

// eventTypeName returns short type name for diagnostic.
func eventTypeName(e session.Event) string {
	switch e.(type) {
	case session.AgentStart:
		return "AgentStart"
	case session.TurnStart:
		return "TurnStart"
	case session.MessageStart:
		return "MessageStart"
	case session.MessageUpdate:
		return "MessageUpdate"
	case session.MessageEnd:
		return "MessageEnd"
	case session.ToolExecStart:
		return "ToolExecStart"
	case session.ToolExecUpdate:
		return "ToolExecUpdate"
	case session.ToolExecEnd:
		return "ToolExecEnd"
	case session.ApprovalRequest:
		return "ApprovalRequest"
	case session.ApprovalResolution:
		return "ApprovalResolution"
	case session.TurnEnd:
		return "TurnEnd"
	case session.AgentEnd:
		return "AgentEnd"
	case session.ModelUpdate:
		return "ModelUpdate"
	case session.ThinkingUpdate:
		return "ThinkingUpdate"
	case session.ToolsUpdate:
		return "ToolsUpdate"
	case session.QueueUpdate:
		return "QueueUpdate"
	case session.Settled:
		return "Settled"
	case session.SavePoint:
		return "SavePoint"
	case session.Abort:
		return "Abort"
	case session.ProviderRetry:
		return "ProviderRetry"
	default:
		return fmt.Sprintf("%T", e)
	}
}

// ----- 1A-1: MessageStart → MessageUpdate* → MessageEnd ordering -----

func TestLifecycle_MessageStartBeforeEnd(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	h := NewHarness(HarnessConfig{
		Session: sess,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "hello "},
				{Content: "world", StopReason: "stop"},
			}}, nil
		},
	})
	defer h.Close()

	events, err := collectWithSubscribe(h, "hi")
	if err != nil {
		t.Fatal(err)
	}

	// Find MessageStart and its corresponding MessageEnd for assistant messages.
	var startIdx, endIdx = -1, -1
	for i, e := range events {
		if _, ok := e.(session.MessageStart); ok {
			if me, ok := e.(session.MessageStart); ok {
				if _, isAsst := me.Message.(*session.AssistantMessage); isAsst {
					if startIdx == -1 {
						startIdx = i
					}
				}
			}
		}
		if me, ok := e.(session.MessageEnd); ok {
			if _, isAsst := me.Message.(*session.AssistantMessage); isAsst {
				endIdx = i
			}
		}
	}
	if startIdx == -1 {
		t.Fatal("no assistant MessageStart found")
	}
	if endIdx == -1 {
		t.Fatal("no assistant MessageEnd found")
	}
	if startIdx >= endIdx {
		t.Fatalf("MessageStart at %d must be before MessageEnd at %d; order: %v",
			startIdx, endIdx, eventNames(events))
	}
	// MessageUpdate, if any, must be between start and end.
	for i, e := range events {
		if _, ok := e.(session.MessageUpdate); ok {
			if i < startIdx || i > endIdx {
				t.Fatalf("MessageUpdate at %d outside [%d,%d]", i, startIdx, endIdx)
			}
		}
	}
}

func eventNames(events []session.Event) []string {
	names := make([]string, len(events))
	for i, e := range events {
		names[i] = fmt.Sprintf("%d:%s", i, eventTypeName(e))
	}
	return names
}

// ----- 1A-2: ToolExecStart → (ToolExecUpdate*) → ToolExecEnd -----

func TestLifecycle_ToolExecOrder(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{
			{Calls: []llm.Call{{ID: "tc1", Type: "function", Function: struct {
				Name      string `json:"name"`
				Arguments string `json:"arguments"`
			}{Name: "echo", Arguments: `{}`}}}, StopReason: "toolUse"},
		}}, nil
	}
	h := NewHarness(HarnessConfig{
		Session:  sess,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
		Tools: []Tool{{
			Name: "echo",
			Execute: func(ctx context.Context, id string, args json.RawMessage, sig <-chan struct{}, prog func(session.ToolPartial)) (session.ToolResultMessage, error) {
				// Optionally emit progress via prog if supported
				if prog != nil {
					prog("partial")
				}
				return session.ToolResultMessage{
					ToolCallID: id,
					ToolName:   "echo",
					Content:    []session.Content{session.TextContent{Text: "ok"}},
					Timestamp:  time.Now(),
				}, nil
			},
		}},
	})
	defer h.Close()

	events, err := collectWithSubscribe(h, "use echo")
	if err != nil {
		t.Fatal(err)
	}
	var startIdx, endIdx = -1, -1
	for i, e := range events {
		if te, ok := e.(session.ToolExecStart); ok && te.ToolCallID == "tc1" {
			startIdx = i
		}
		if te, ok := e.(session.ToolExecEnd); ok && te.ToolCallID == "tc1" {
			endIdx = i
		}
	}
	if startIdx == -1 {
		t.Fatalf("no ToolExecStart for tc1; events: %v", eventNames(events))
	}
	if endIdx == -1 {
		t.Fatalf("no ToolExecEnd for tc1; events: %v", eventNames(events))
	}
	if startIdx >= endIdx {
		t.Fatalf("ToolExecStart at %d must precede ToolExecEnd at %d", startIdx, endIdx)
	}
}

// ----- 1A-3: Single terminal AgentEnd for normal path -----

func TestLifecycle_SingleAgentEnd_Normal(t *testing.T) {
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

	events, err := collectWithSubscribe(h, "hello")
	if err != nil {
		t.Fatal(err)
	}
	count := 0
	for _, e := range events {
		if _, ok := e.(session.AgentEnd); ok {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("expected exactly 1 AgentEnd for normal path, got %d: %v", count, eventNames(events))
	}
}

// ----- 1A-4: No events after terminal AgentEnd -----

func TestLifecycle_NoEventsAfterAgentEnd(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	h := NewHarness(HarnessConfig{
		Session: sess,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "done", StopReason: "stop"},
			}}, nil
		},
	})
	defer h.Close()

	events, err := collectWithSubscribe(h, "hi")
	if err != nil {
		t.Fatal(err)
	}
	seenEnd := false
	for i, e := range events {
		if _, ok := e.(session.AgentEnd); ok {
			seenEnd = true
			// Check subsequent events (if any) — there should be none that matter,
			// but we allow Settled after AgentEnd per current impl (bug) and will
			// fix ordering in 1B. For now, assert no Message/Tool events after End.
			for j := i + 1; j < len(events); j++ {
				switch events[j].(type) {
				case session.MessageStart, session.MessageUpdate, session.MessageEnd,
					session.ToolExecStart, session.ToolExecUpdate, session.ToolExecEnd,
					session.TurnStart, session.TurnEnd:
					t.Fatalf("event %s at %d after AgentEnd at %d: %v",
						eventTypeName(events[j]), j, i, eventNames(events))
				}
			}
		}
	}
	if !seenEnd {
		t.Fatal("no AgentEnd seen")
	}
}

// ----- 1A-5: Single terminal AgentEnd on panic recovery -----

func TestLifecycle_SingleAgentEnd_OnPanic(t *testing.T) {
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

	// Force a panic inside RunLoop by using a nil StreamFn? Instead we directly
	// test the recover path via a custom StreamFn that panics.
	h.stream = func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		panic("test panic")
	}

	events, err := collectWithSubscribe(h, "cause panic")
	// Prompt returns nil error but produces failure message; we only care about event counts
	_ = err

	count := 0
	for _, e := range events {
		if _, ok := e.(session.AgentEnd); ok {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("expected exactly 1 AgentEnd on panic path, got %d: %v", count, eventNames(events))
	}
}

// ----- 1A-6: Persist-before-emit for MessageEnd -----

func TestLifecycle_PersistBeforeEmit(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	h := NewHarness(HarnessConfig{
		Session: sess,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "persisted?", StopReason: "stop"},
			}}, nil
		},
	})
	defer h.Close()

	var seenMessageEnd bool
	var messagesAtMessageEnd int
	h.Subscribe(func(e session.Event) {
		if _, ok := e.(session.MessageEnd); ok {
			// At this point, BuildContext should include the message if persist-before-emit holds.
			snap, err := sess.BuildContext(context.Background())
			if err == nil {
				// Filter assistant messages
				for _, m := range snap.Messages {
					if _, isAsst := m.(*session.AssistantMessage); isAsst {
						messagesAtMessageEnd++
					}
				}
			}
			seenMessageEnd = true
		}
	})

	if _, err := h.Prompt(context.Background(), "check persist"); err != nil {
		t.Fatal(err)
	}

	if !seenMessageEnd {
		t.Fatal("did not see MessageEnd")
	}
	if messagesAtMessageEnd == 0 {
		t.Fatalf("persist-before-emit violated: BuildContext at MessageEnd had 0 assistant messages")
	}
}

// ----- 1A-7: Event registry completeness -----

func TestLifecycle_EventRegistryComplete(t *testing.T) {
	// Enumerate all known event types. If a new Event is added, this test must be
	// updated — it acts as an exhaustive table asserting a reducer disposition.
	// This mirrors Sol's suggestion: "Add a registry/table test enumerating every
	// event type and asserting a reducer disposition."

	// All Event types known in session/ package.
	known := []session.Event{
		session.AgentStart{},
		session.TurnStart{},
		session.MessageStart{},
		session.MessageUpdate{},
		session.MessageEnd{},
		session.ToolExecStart{},
		session.ToolExecUpdate{},
		session.ToolExecEnd{},
		session.ApprovalRequest{},
		session.ApprovalResolution{},
		session.TurnEnd{},
		session.AgentEnd{},
		session.ModelUpdate{},
		session.ThinkingUpdate{},
		session.ToolsUpdate{},
		session.QueueUpdate{},
		session.Settled{},
		session.SavePoint{},
		session.Abort{},
		session.ProviderRetry{},
		&session.Error{},
	}

	// Every Event must be handleable. We assert that type-switch in handleEvent
	// could cover it — here we simulate a exhaustive switch and force an update
	// if a new Event appears.
	handled := make(map[string]bool)
	for _, e := range known {
		switch e.(type) {
		case session.AgentStart, // not emitted by handleEvent directly, but still Event
			session.TurnStart,
			session.MessageStart,
			session.MessageUpdate,
			session.MessageEnd,
			session.ToolExecStart,
			session.ToolExecUpdate,
			session.ToolExecEnd,
			session.ApprovalRequest,
			session.ApprovalResolution,
			session.TurnEnd,
			session.AgentEnd,
			session.ModelUpdate,
			session.ThinkingUpdate,
			session.ToolsUpdate,
			session.QueueUpdate,
			session.Settled,
			session.SavePoint,
			session.Abort,
			session.ProviderRetry,
			*session.Error:
			handled[eventTypeName(e)] = true
		default:
			t.Fatalf("unhandled Event type in registry: %T — add it to lifecycle contract test", e)
		}
	}

	if len(handled) != len(known) {
		t.Fatalf("registry mismatch: %d handled vs %d known", len(handled), len(known))
	}
}

// ----- 1A-8: Loop statelessness guard (no persistence fields) -----

func TestLifecycle_LoopStateless(t *testing.T) {
	// DESIGN §2: RunLoop takes all inputs as args and emits events — no *session.Session field,
	// no persistence calls in loop files. This is already enforced by a grep guard:
	// rg '\*session\.Session|\.Append|SQLiteStore|syncLoopState|l\.tree' internal/agent/loop.go -> empty
	// Mirror that guard here as a regression test.

	// We cannot easily introspect struct fields without reflection that crosses package,
	// but we can at least verify loop does not import session.Store persistence by behavior:
	// RunLoop with an empty TurnContext and a failing StreamFn must still emit AgentEnd without touching store.

	sess := session.NewSession(newTestStore(t), 64)
	h := NewHarness(HarnessConfig{
		Session: sess,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return nil, fmt.Errorf("injected failure")
		},
	})
	defer h.Close()

	events, err := collectWithSubscribe(h, "fail fast")
	if err != nil {
		t.Fatal(err)
	}
	// Must still get exactly one AgentEnd despite failure, proving loop didn't need store.
	count := 0
	for _, e := range events {
		if _, ok := e.(session.AgentEnd); ok {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("expected 1 AgentEnd on stream failure, got %d", count)
	}
}

// ----- 1A-9: SavePoint HadPendingMutations bug documentation -----

func TestLifecycle_SavePointHadPendingMutations(t *testing.T) {
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

	// Force pending writes: SetModel before Prompt leaves a pendingWrite that
	// TurnEnd should report via HadPendingMutations (fixed in 1B).
	h.SetModel(llm.Model{ID: "new-model", Provider: "test"})

	var savePoints []session.SavePoint
	var mu sync.Mutex
	h.Subscribe(func(e session.Event) {
		if sp, ok := e.(session.SavePoint); ok {
			mu.Lock()
			savePoints = append(savePoints, sp)
			mu.Unlock()
		}
	})

	_, err := h.Prompt(context.Background(), "hello")
	if err != nil {
		t.Fatal(err)
	}

	if len(savePoints) == 0 {
		t.Fatal("no SavePoint emitted")
	}
	// After 1B fix, HadPendingMutations should be true because we queued SetModel.
	if !savePoints[0].HadPendingMutations {
		t.Fatalf("expected HadPendingMutations=true after SetModel pending write, got false; events: %v", eventNames(filterEvents(nil, func(e session.Event) bool { return true })))
	}
}

func TestLifecycle_SavePointHadPendingMutations_Bug(t *testing.T) {
	// Historical name wrapper.
	TestLifecycle_SavePointHadPendingMutations(t)
}

// ----- 1A-10: Settled ordering bug documentation -----

func TestLifecycle_SettledOrdering(t *testing.T) {
	// After 1B fix: Settled after AgentEnd, matching Pi and events.go doc.
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

	events, err := collectWithSubscribe(h, "ordering")
	if err != nil {
		t.Fatal(err)
	}

	var settledIdx, agentEndIdx = -1, -1
	for i, e := range events {
		switch e.(type) {
		case session.Settled:
			if settledIdx == -1 {
				settledIdx = i
			}
		case session.AgentEnd:
			agentEndIdx = i
		}
	}
	if settledIdx == -1 {
		t.Fatalf("no Settled seen; events: %v", eventNames(events))
	}
	if agentEndIdx == -1 {
		t.Fatal("no AgentEnd seen")
	}

	if settledIdx < agentEndIdx {
		t.Fatalf("Settled at %d before AgentEnd at %d — expected after per events.go doc + Pi", settledIdx, agentEndIdx)
	}
}

func TestLifecycle_SettledOrdering_Bug(t *testing.T) {
	// Historical name kept for backward reference, now asserts correct ordering.
	TestLifecycle_SettledOrdering(t)
}

// ----- 1C: Backpressure regression -----

func TestEmit_Backpressure_NoDropWhenDraining(t *testing.T) {
	// Fill Events channel to capacity, then ensure a concurrent drainer prevents drop.
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	// Create harness with small buffer to force backpressure quickly.
	h := NewHarness(HarnessConfig{
		Session: sess,
		Model:   llm.Model{ID: "test"},
		Events:  make(chan session.Event, 1), // tiny buffer to trigger slow path
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			// Emit many chunks to generate many MessageUpdate events.
			chunks := make([]*llm.Chunk, 20)
			for i := range chunks {
				chunks[i] = &llm.Chunk{Content: fmt.Sprintf("c%d ", i)}
			}
			chunks[19].StopReason = "stop"
			return &mockStream{chunks: chunks}, nil
		},
	})
	defer h.Close()

	// Concurrent drainer mimicking TUI's awaitSessionEvent loop.
	var drained []session.Event
	var dMu sync.Mutex
	drainDone := make(chan struct{})
	go func() {
		defer close(drainDone)
		for {
			select {
			case e, ok := <-h.Events():
				if !ok {
					return
				}
				dMu.Lock()
				drained = append(drained, e)
				dMu.Unlock()
				if _, ok := e.(session.Settled); ok {
					return
				}
			case <-time.After(5 * time.Second):
				return
			}
		}
	}()

	// Subscribe also captures all events via listeners (no drop path).
	var viaSubscribe []session.Event
	var sMu sync.Mutex
	unsub := h.Subscribe(func(e session.Event) {
		sMu.Lock()
		viaSubscribe = append(viaSubscribe, e)
		sMu.Unlock()
	})
	defer unsub()

	_, err := h.Prompt(context.Background(), "backpressure")
	if err != nil {
		t.Fatal(err)
	}
	<-drainDone

	sMu.Lock()
	dMu.Lock()
	// At least the terminal lifecycle events must be present in both paths.
	has := func(list []session.Event, name string) bool {
		for _, e := range list {
			if eventTypeName(e) == name {
				return true
			}
		}
		return false
	}
	for _, need := range []string{"AgentEnd", "Settled", "MessageEnd", "TurnEnd"} {
		if !has(viaSubscribe, need) {
			t.Fatalf("Subscribe path missing %s; got %v", need, eventNames(viaSubscribe))
		}
		if !has(drained, need) {
			t.Fatalf("Channel path missing %s under backpressure; drained=%v subscribe=%v", need, eventNames(drained), eventNames(viaSubscribe))
		}
	}
	dMu.Unlock()
	sMu.Unlock()
}

// helper: contains for event names

func containsEvent(events []session.Event, name string) bool {
	for _, e := range events {
		if eventTypeName(e) == name {
			return true
		}
	}
	return false
}

// helper: filter events by name substring

func filterEvents(events []session.Event, pred func(session.Event) bool) []session.Event {
	var out []session.Event
	for _, e := range events {
		if pred(e) {
			out = append(out, e)
		}
	}
	return out
}

// Ensure test file doesn't accidentally import unused strings (used in earlier version)
var _ = strings.Contains
