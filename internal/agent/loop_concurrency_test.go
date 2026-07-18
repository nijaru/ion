package agent

import (
	"context"
	"encoding/json"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestParallelToolCallsBoundWorkersAndPreserveEventOrder(t *testing.T) {
	var active, maximum int32
	tool := Tool{
		Name:          "bounded",
		ExecutionMode: ExecParallel,
		Parameters:    `{"type":"object"}`,
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			now := atomic.AddInt32(&active, 1)
			for {
				old := atomic.LoadInt32(&maximum)
				if now <= old || atomic.CompareAndSwapInt32(&maximum, old, now) {
					break
				}
			}
			time.Sleep(10 * time.Millisecond)
			atomic.AddInt32(&active, -1)
			return session.ToolResultMessage{}, nil
		},
	}
	calls := make([]*session.ToolCall, 6)
	for i := range calls {
		calls[i] = &session.ToolCall{ID: string(rune('a' + i)), Name: tool.Name, Arguments: map[string]any{}}
	}
	var events []session.Event
	results, _ := executeToolCallsParallel(
		context.Background(), TurnContext{}, session.AssistantMessage{}, calls,
		LoopConfig{Tools: []Tool{tool}, MaxParallelTools: 2},
		func(event session.Event) { events = append(events, event) }, nil,
	)
	if len(results) != len(calls) {
		t.Fatalf("results = %d, want %d", len(results), len(calls))
	}
	if got := atomic.LoadInt32(&maximum); got > 2 {
		t.Fatalf("maximum concurrent executions = %d, want <= 2", got)
	}
	for i, call := range calls {
		start, end := -1, -1
		for j, event := range events {
			switch event := event.(type) {
			case session.ToolExecStart:
				if event.ToolCallID == call.ID {
					start = j
				}
			case session.ToolExecEnd:
				if event.ToolCallID == call.ID {
					end = j
				}
			}
		}
		if start < 0 || end <= start {
			t.Fatalf("call %d event order start=%d end=%d", i, start, end)
		}
	}
	lastEnd := -1
	for _, call := range calls {
		for j, event := range events {
			if end, ok := event.(session.ToolExecEnd); ok && end.ToolCallID == call.ID {
				if j < lastEnd {
					t.Fatalf("tool end order is not deterministic at %q", call.ID)
				}
				lastEnd = j
			}
		}
	}
}
