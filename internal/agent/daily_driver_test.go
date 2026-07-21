package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// TestDailyDriverSubmitToolPersistReplay proves the deterministic spine across
// a real SQLite close/reopen: submit, stream a tool call, persist its result,
// reconstruct context, and submit again from the resumed leaf.
func TestDailyDriverSubmitToolPersistReplay(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "session.db")
	store, err := session.NewSQLiteStore(path, "daily-driver")
	if err != nil {
		t.Fatal(err)
	}

	sess := session.NewSession(store, 64)
	tool := Tool{
		Name:        "echo",
		Description: "Echo input",
		Parameters:  `{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}`,
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{
				ToolCallID: "call_daily",
				ToolName:   "echo",
				Content:    []session.Content{session.TextContent{Text: "tool-result"}},
				Terminate:  true,
			}, nil
		},
	}

	events := make(chan session.Event, 64)
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "test"},
		Tools:   []Tool{tool},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Calls: []llm.Call{{
					ID:   "call_daily",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "echo", Arguments: `{"text":"hello"}`},
				}}},
				{StopReason: "stop"},
			}}, nil
		},
	})
	unsub := watchEvents(t, h, func(event session.Event) { events <- event })
	defer unsub()
	if _, err := h.Prompt(ctx, "use echo"); err != nil {
		t.Fatalf("first Prompt: %v", err)
	}
	var eventTypes []string
	for {
		select {
		case event := <-events:
			eventTypes = append(eventTypes, fmt.Sprintf("%T", event))
			if _, ok := event.(session.AgentEnd); ok {
				goto collected
			}
		case <-time.After(time.Second):
			t.Fatal("timed out collecting first turn events")
		}
	}

collected:
	if !containsEventType(eventTypes, "ToolExecStart") || !containsEventType(eventTypes, "ToolExecEnd") {
		t.Fatalf("tool lifecycle missing from events: %v", eventTypes)
	}
	if err := h.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close first store: %v", err)
	}

	store2, err := session.NewSQLiteStore(path, "daily-driver")
	if err != nil {
		t.Fatal(err)
	}
	leaf := store2.GetLeafID()
	if leaf == "" {
		t.Fatal("reopened store has no persisted leaf")
	}
	if err := store2.ResumeSession(ctx, leaf); err != nil {
		t.Fatalf("resume persisted leaf: %v", err)
	}
	sess2 := session.NewSession(store2, 64)
	snapshot, err := sess2.BuildContext(ctx)
	if err != nil {
		t.Fatalf("BuildContext after reopen: %v", err)
	}
	if !hasText(snapshot.Messages, "tool-result") {
		t.Fatalf("replayed context missing persisted tool result: %v", messageTypeList(snapshot.Messages))
	}

	h2 := NewController(ControllerConfig{
		Session: sess2,
		Store:   store2,
		Durable: store2,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "resumed", StopReason: "stop"}}}, nil
		},
	})
	msg, err := h2.Prompt(ctx, "continue after resume")
	if err != nil {
		t.Fatalf("resumed Prompt: %v", err)
	}
	am, ok := msg.(*session.AssistantMessage)
	if !ok {
		t.Fatalf("resumed response type = %T, want *AssistantMessage", msg)
	}
	if got := textContentMsg(am); got != "resumed" {
		t.Fatalf("resumed response = %q, want %q", got, "resumed")
	}
	if err := h2.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if err := store2.Close(); err != nil {
		t.Fatalf("close second store: %v", err)
	}
}

func containsEventType(types []string, want string) bool {
	for _, typ := range types {
		if strings.Contains(typ, want) {
			return true
		}
	}
	return false
}

func hasText(messages []session.Message, want string) bool {
	for _, message := range messages {
		var text string
		switch message := message.(type) {
		case *session.AssistantMessage:
			text = textContentMsg(message)
		case *session.ToolResultMessage:
			text = textOfFirst(message.Content)
		case *session.UserMessage:
			text = textOfFirst(message.Content)
		}
		if strings.Contains(text, want) {
			return true
		}
	}
	return false
}

func messageTypeList(messages []session.Message) []string {
	types := make([]string, len(messages))
	for i, message := range messages {
		types[i] = msgTypeName(message)
	}
	return types
}
