package agent

import (
	"context"
	"encoding/json"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestParallelToolProgressPublishesBeforeCompletion(t *testing.T) {
	progressStarted := make(chan struct{}, 2)
	release := make(chan struct{})
	tool := Tool{
		Name:       "progress",
		Parameters: `{"type":"object"}`,
		Execute: func(_ context.Context, _ string, _ json.RawMessage, _ <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			progress("still working")
			progressStarted <- struct{}{}
			<-release
			return session.ToolResultMessage{Content: []session.Content{session.TextContent{Text: "done"}}}, nil
		},
	}
	calls := []*session.ToolCall{
		{ID: "progress-1", Name: tool.Name, Arguments: map[string]any{}},
		{ID: "progress-2", Name: tool.Name, Arguments: map[string]any{}},
	}
	var events []session.Event
	progressObserved := make(chan struct{}, 1)
	done := make(chan struct{})
	go func() {
		_, _ = executeToolCallsParallel(
			context.Background(), TurnContext{}, session.AssistantMessage{}, calls,
			LoopConfig{Tools: []Tool{tool}, MaxParallelTools: 2},
			func(event session.Event) {
				events = append(events, event)
				if _, ok := event.(session.ToolExecUpdate); ok {
					select {
					case progressObserved <- struct{}{}:
					default:
					}
				}
			}, nil,
		)
		close(done)
	}()
	for range 2 {
		select {
		case <-progressStarted:
		case <-time.After(time.Second):
			t.Fatal("tool did not start")
		}
	}
	select {
	case <-progressObserved:
	case <-time.After(time.Second):
		t.Fatal("tool progress was buffered until completion")
	}
	close(release)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("parallel tool did not finish")
	}
}

func TestParallelToolProgressStopsBeforeToolEnd(t *testing.T) {
	liveStarted := make(chan struct{})
	release := make(chan struct{})
	lateTrigger := make(chan struct{})
	lateDone := make(chan struct{})
	tool := Tool{
		Name:       "late-progress",
		Parameters: `{"type":"object"}`,
		Execute: func(_ context.Context, _ string, _ json.RawMessage, _ <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			go func() {
				defer close(lateDone)
				<-lateTrigger
				progress("late")
			}()
			progress("live")
			close(liveStarted)
			<-release
			return session.ToolResultMessage{}, nil
		},
	}
	events := make(chan session.Event, 8)
	done := make(chan struct{})
	go func() {
		call := &session.ToolCall{ID: "late-1", Name: tool.Name, Arguments: map[string]any{}}
		_, _ = executeToolCallsParallel(
			context.Background(), TurnContext{}, session.AssistantMessage{}, []*session.ToolCall{call},
			LoopConfig{Tools: []Tool{tool}, MaxParallelTools: 1},
			func(event session.Event) { events <- event }, nil,
		)
		close(done)
	}()
	select {
	case <-liveStarted:
	case <-time.After(time.Second):
		t.Fatal("tool did not start")
	}
	close(release)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("tool did not finish")
	}
	close(lateTrigger)
	<-lateDone

	ended := false
	updates := 0
	for {
		select {
		case event := <-events:
			switch event.(type) {
			case session.ToolExecUpdate:
				if ended {
					t.Fatal("tool progress arrived after ToolExecEnd")
				}
				updates++
			case session.ToolExecEnd:
				ended = true
			}
		default:
			if !ended {
				t.Fatal("tool execution did not emit ToolExecEnd")
			}
			if updates != 1 {
				t.Fatalf("ToolExecUpdate count = %d, want only live update", updates)
			}
			return
		}
	}
}

func TestParallelToolPreparationFailuresPreserveTerminalOrder(t *testing.T) {
	tool := Tool{
		Name:       "valid",
		Parameters: `{"type":"object"}`,
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{ToolCallID: "valid-1"}, nil
		},
	}
	calls := []*session.ToolCall{
		{ID: "valid-1", Name: "valid", Arguments: map[string]any{}},
		{ID: "missing-1", Name: "missing", Arguments: map[string]any{}},
	}
	var ends []string
	_, _ = executeToolCallsParallel(
		context.Background(), TurnContext{}, session.AssistantMessage{}, calls,
		LoopConfig{Tools: []Tool{tool}, MaxParallelTools: 2},
		func(event session.Event) {
			if end, ok := event.(session.ToolExecEnd); ok {
				ends = append(ends, end.ToolCallID)
			}
		}, nil,
	)
	if len(ends) != 2 || ends[0] != "valid-1" || ends[1] != "missing-1" {
		t.Fatalf("tool end order = %#v, want valid-1 then missing-1", ends)
	}
}

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
