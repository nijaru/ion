package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func BenchmarkExecuteToolCallsParallel8(b *testing.B) {
	tool := Tool{
		Name:          "benchmark-echo",
		ExecutionMode: ExecParallel,
		Parameters:    `{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}`,
		Execute: func(_ context.Context, id string, _ json.RawMessage, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{
				ToolCallID: id,
				ToolName:   "benchmark-echo",
				Content:    []session.Content{session.TextContent{Text: "ok"}},
				Timestamp:  time.Now(),
			}, nil
		},
	}
	calls := make([]*session.ToolCall, 8)
	for i := range calls {
		calls[i] = &session.ToolCall{
			ID:        fmt.Sprintf("call-%d", i),
			Name:      tool.Name,
			Arguments: map[string]any{"value": "benchmark"},
		}
	}
	cfg := LoopConfig{Tools: []Tool{tool}, MaxParallelTools: 8}
	signal := make(chan struct{})

	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		results, _ := executeToolCallsParallel(
			context.Background(), TurnContext{}, session.AssistantMessage{}, calls, cfg,
			func(session.Event) {}, signal,
		)
		if len(results) != len(calls) {
			b.Fatalf("results = %d, want %d", len(results), len(calls))
		}
	}
}

func BenchmarkControllerSubscribeSnapshot256(b *testing.B) {
	store, err := session.NewSQLiteStore(":memory:", "subscription-benchmark")
	if err != nil {
		b.Fatal(err)
	}
	defer store.Close()

	ctx := b.Context()
	parentID := ""
	for i := 0; i < 256; i++ {
		id := fmt.Sprintf("entry-%03d", i)
		entry := &session.MessageEntry{
			EntryBase: session.EntryBase{
				ID:        id,
				ParentID:  parentID,
				Timestamp: time.UnixMilli(int64(i)),
			},
			Message: session.NewUserText(fmt.Sprintf("message %d", i), time.UnixMilli(int64(i))),
		}
		if _, err := store.AppendLeafEntry(ctx, entry); err != nil {
			b.Fatal(err)
		}
		parentID = id
	}

	controller := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
		Model:   llm.Model{ID: "benchmark-model"},
	})
	defer controller.Close()

	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		subscription, err := controller.Subscribe(ctx, EventCursor{})
		if err != nil {
			b.Fatal(err)
		}
		subscription.Close()
	}
}

func BenchmarkControllerClose(b *testing.B) {
	store, err := session.NewSQLiteStore(":memory:", "close-benchmark")
	if err != nil {
		b.Fatal(err)
	}
	defer store.Close()

	cfg := ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
		Model:   llm.Model{ID: "benchmark-model"},
	}
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		controller := NewController(cfg)
		if err := controller.Close(); err != nil {
			b.Fatal(err)
		}
	}
}
