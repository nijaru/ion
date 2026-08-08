package agent

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/nijaru/ion/session"
)

func TestEmitHookRunsAllHandlersAndJoinsErrors(t *testing.T) {
	h := NewController(ControllerConfig{Session: session.NewSession(newTestStore(t), 64)})
	defer func() { _ = h.Close() }()

	first := errors.New("first hook failed")
	second := errors.New("second hook failed")
	calls := 0
	h.On("test", func(any) (any, error) {
		calls++
		return "ignored", first
	})
	h.On("test", func(any) (any, error) {
		calls++
		return nil, second
	})

	patches, err := h.emitHook("test", nil)
	if calls != 2 {
		t.Fatalf("hook calls = %d, want 2", calls)
	}
	if len(patches) != 0 {
		t.Fatalf("patches = %#v, want none from failed handlers", patches)
	}
	if !errors.Is(err, first) || !errors.Is(err, second) {
		t.Fatalf("joined error = %v, want both hook errors", err)
	}
}

func TestEmitHookConvertsPanicsToErrors(t *testing.T) {
	h := NewController(ControllerConfig{Session: session.NewSession(newTestStore(t), 64)})
	defer func() { _ = h.Close() }()

	calledAfterPanic := false
	h.On("test", func(any) (any, error) {
		panic("hook exploded")
	})
	h.On("test", func(any) (any, error) {
		calledAfterPanic = true
		return nil, nil
	})
	if _, err := h.emitHook("test", nil); err == nil || !strings.Contains(err.Error(), "hook panic: hook exploded") {
		t.Fatalf("panic error = %v, want recovered hook panic", err)
	}
	if !calledAfterPanic {
		t.Fatal("hook dispatch stopped after recovering a panic")
	}
}

func TestHookUnsubscribeSurvivesLaterRegistration(t *testing.T) {
	h := NewController(ControllerConfig{Session: session.NewSession(newTestStore(t), 64)})
	defer func() { _ = h.Close() }()

	firstCalls := 0
	unsubscribe := h.On("test", func(any) (any, error) {
		firstCalls++
		return nil, nil
	})
	secondCalls := 0
	h.On("test", func(any) (any, error) {
		secondCalls++
		return nil, nil
	})

	unsubscribe()
	unsubscribe()
	if _, err := h.emitHook("test", nil); err != nil {
		t.Fatal(err)
	}
	if firstCalls != 0 {
		t.Fatalf("unsubscribed hook calls = %d, want 0", firstCalls)
	}
	if secondCalls != 1 {
		t.Fatalf("remaining hook calls = %d, want 1", secondCalls)
	}
}

func TestBeforeToolCallHookPanicBlocksExecution(t *testing.T) {
	h := NewController(ControllerConfig{Session: session.NewSession(newTestStore(t), 64)})
	defer func() { _ = h.Close() }()

	h.On(HookBeforeToolCall, func(any) (any, error) {
		panic("tool policy hook exploded")
	})
	cfg := h.buildLoopConfig(context.Background(), nil, nil)
	decision := cfg.BeforeToolCall(ToolCallContext{
		RunContext: context.Background(),
		ToolCall:   &session.ToolCall{ID: "call-1", Name: "write"},
	})
	if decision == nil || !decision.Block {
		t.Fatalf("decision = %#v, want blocking decision", decision)
	}
	if !strings.Contains(decision.Reason, "before_tool_call hook: hook panic: tool policy hook exploded") {
		t.Fatalf("decision reason = %q, want recovered hook error", decision.Reason)
	}
}

func TestHarnessAfterToolCallHookErrorReachesLoopPatch(t *testing.T) {
	h := NewController(ControllerConfig{Session: session.NewSession(newTestStore(t), 64)})
	defer func() { _ = h.Close() }()

	h.On(HookToolResult, func(any) (any, error) {
		return nil, errors.New("tool result hook failed")
	})
	cfg := h.buildLoopConfig(context.Background(), nil, nil)
	patch := cfg.AfterToolCall(ToolCallResultContext{
		ToolCall: &session.ToolCall{ID: "call-1", Name: "read"},
		Result: session.ToolResultMessage{
			ToolCallID: "call-1",
			ToolName:   "read",
		},
	})
	if patch == nil || patch.Error == nil {
		t.Fatalf("patch = %#v, want propagated hook error", patch)
	}
	if !strings.Contains(patch.Error.Error(), "tool result hook failed") {
		t.Fatalf("patch error = %v, want hook message", patch.Error)
	}
}
