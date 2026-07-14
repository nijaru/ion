package agent

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/nijaru/ion/session"
)

func TestEmitHookRunsAllHandlersAndJoinsErrors(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: session.NewSession(newTestStore(t), 64)})
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

func TestHarnessAfterToolCallHookErrorReachesLoopPatch(t *testing.T) {
	h := NewHarness(HarnessConfig{Session: session.NewSession(newTestStore(t), 64)})
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
