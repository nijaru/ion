package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// TestPrintModeUsesTheDurableRuntime proves that the host print path observes
// the same Controller and SQLite lifecycle as the TUI: streamed output is
// rendered before Prompt returns, and the settled turn survives a reopen.
func TestPrintModeUsesTheDurableRuntime(t *testing.T) {
	ctx := t.Context()
	path := filepath.Join(t.TempDir(), "print.db")
	store, err := session.NewSQLiteStore(path, "print-acceptance")
	if err != nil {
		t.Fatalf("open print store: %v", err)
	}
	sess := session.NewSession(store, 64)
	runner := agent.NewController(agent.ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "fake-model", Provider: "fake"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &printAcceptanceStream{chunks: []*llm.Chunk{
				{Content: "hello"},
				{Content: ", world", StopReason: "stop"},
			}}, nil
		},
	})
	closed := false
	t.Cleanup(func() {
		if closed {
			return
		}
		if err := runner.Close(); err != nil {
			t.Errorf("cleanup runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("cleanup store: %v", err)
		}
	})

	var out bytes.Buffer
	if err := runPrintModeWithWriter(ctx, &out, runner, "say hello", "text"); err != nil {
		t.Fatalf("run print mode: %v", err)
	}
	if got := out.String(); got != "hello, world\n" {
		t.Fatalf("print output = %q, want streamed response", got)
	}
	leafID := store.GetLeafID()
	if leafID == "" {
		t.Fatal("print turn did not materialize a session leaf")
	}
	if err := updatePrintSessionInfo(
		ctx,
		runner,
		t.TempDir(),
		"main",
		&config.Config{Provider: "fake", Model: "fake-model"},
		"say hello",
	); err != nil {
		t.Fatalf("persist print session catalog: %v", err)
	}
	sessionID := runner.SessionID()
	info, err := store.GetSessionInfo(ctx, sessionID)
	if err != nil {
		t.Fatalf("load print session catalog by stable ID: %v", err)
	}
	if info.ID() != sessionID || info.LeafID != leafID || info.LastPreview != "say hello" {
		t.Fatalf("print session catalog = %#v, want stable ID %q, leaf %q, and prompt preview", info, sessionID, leafID)
	}

	if err := runner.Close(); err != nil {
		t.Fatalf("close runtime: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close store: %v", err)
	}
	closed = true

	reopened, err := session.NewSQLiteStore(path, "ignored")
	if err != nil {
		t.Fatalf("reopen print store: %v", err)
	}
	defer reopened.Close()
	replayed := session.NewSession(reopened, 64)
	snapshot, err := replayed.BuildContext(ctx)
	if err != nil {
		t.Fatalf("build replay context: %v", err)
	}
	if len(snapshot.Messages) != 2 {
		t.Fatalf("replayed message count = %d, want 2", len(snapshot.Messages))
	}
	if got := session.MessageText(snapshot.Messages[0]); got != "say hello" {
		t.Fatalf("replayed prompt = %q, want say hello", got)
	}
	if got := session.MessageText(snapshot.Messages[1]); got != "hello, world" {
		t.Fatalf("replayed response = %q, want hello, world", got)
	}
}

func TestStructuredPrintModeWaitsForTerminalSettlementAfterToolLoop(t *testing.T) {
	ctx := t.Context()
	path := filepath.Join(t.TempDir(), "tool-loop-events.db")
	store, err := session.NewSQLiteStore(path, "tool-loop-events")
	if err != nil {
		t.Fatalf("open events store: %v", err)
	}
	sess := session.NewSession(store, 64)
	calls := 0
	runner := agent.NewController(agent.ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "fake-model", Provider: "fake"},
		Tools: []agent.Tool{
			{
				Name: "echo",
				Execute: func(_ context.Context, id string, _ json.RawMessage, _ <-chan struct{}, _ func(session.ToolPartial)) (
					session.ToolResultMessage,
					error,
				) {
					return session.ToolResultMessage{
						ToolCallID: id,
						ToolName:   "echo",
						Content:    []session.Content{session.TextContent{Text: "tool result"}},
					}, nil
				},
			},
		},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			calls++
			if calls == 1 {
				return &printAcceptanceStream{chunks: []*llm.Chunk{{
					Block:      llm.ToolCallBlock{ID: "call-1", Name: "echo", Arguments: `{}`},
					StopReason: llm.StopReasonToolUse,
				}}}, nil
			}
			return &printAcceptanceStream{
				chunks: []*llm.Chunk{{Content: "after tool", StopReason: llm.StopReasonStop}},
			}, nil
		},
	})
	defer func() {
		if err := runner.Close(); err != nil {
			t.Errorf("close runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("close store: %v", err)
		}
	}()

	var out bytes.Buffer
	if err := runPrintModeWithWriter(ctx, &out, runner, "use echo", "events"); err != nil {
		t.Fatalf("run structured tool-loop print mode: %v", err)
	}

	var events []structuredPrintEvent
	decoder := json.NewDecoder(&out)
	for {
		var event structuredPrintEvent
		err := decoder.Decode(&event)
		if err == io.EOF {
			break
		}
		if err != nil {
			t.Fatalf("decode structured output %q: %v", out.String(), err)
		}
		events = append(events, event)
	}
	if len(events) == 0 || events[len(events)-1].Type != "result" {
		t.Fatalf("structured tool-loop events = %v, want result last", eventTypes(events))
	}
	settledIndex := -1
	turnEndCount := 0
	for index, event := range events[:len(events)-1] {
		switch event.Type {
		case "turn_end":
			turnEndCount++
		case "settled":
			settledIndex = index
		}
	}
	if turnEndCount != 2 {
		t.Fatalf("turn_end count = %d, want tool iteration plus final turn", turnEndCount)
	}
	if settledIndex < 0 {
		t.Fatalf("structured tool-loop events omitted settled lifecycle: %v", eventTypes(events))
	}
	for index, event := range events[:len(events)-1] {
		if event.Type == "turn_end" && index > settledIndex {
			t.Fatalf("turn_end after settled at index %d: %v", index, eventTypes(events))
		}
	}
}

func eventTypes(events []structuredPrintEvent) []string {
	types := make([]string, 0, len(events))
	for _, event := range events {
		types = append(types, event.Type)
	}
	return types
}

func TestStructuredPrintModeWaitsForTerminalSettlementOnAcceptedFailure(t *testing.T) {
	ctx := t.Context()
	path := filepath.Join(t.TempDir(), "failed-events.db")
	store, err := session.NewSQLiteStore(path, "failed-events")
	if err != nil {
		t.Fatalf("open events store: %v", err)
	}
	sess := session.NewSession(store, 64)
	runner := agent.NewController(agent.ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "fake-model", Provider: "fake"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return nil, errors.New("provider unavailable")
		},
	})
	defer func() {
		if err := runner.Close(); err != nil {
			t.Errorf("close runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("close store: %v", err)
		}
	}()

	var out bytes.Buffer
	err = runPrintModeWithWriter(ctx, &out, runner, "fail", "events")
	if err == nil || !strings.Contains(err.Error(), "submit turn") {
		t.Fatalf("failed structured print error = %v, want submit failure", err)
	}

	var events []structuredPrintEvent
	decoder := json.NewDecoder(&out)
	for {
		var event structuredPrintEvent
		err := decoder.Decode(&event)
		if err == io.EOF {
			break
		}
		if err != nil {
			t.Fatalf("decode failed structured output %q: %v", out.String(), err)
		}
		events = append(events, event)
	}
	if len(events) == 0 || events[len(events)-1].Type != "error" {
		t.Fatalf("failed structured events = %v, want final error", eventTypes(events))
	}
	settledIndex := -1
	agentEndIndex := -1
	for index, event := range events[:len(events)-1] {
		switch event.Type {
		case "settled":
			settledIndex = index
		case "agent_end":
			agentEndIndex = index
		}
	}
	if agentEndIndex < 0 || settledIndex < 0 || agentEndIndex > settledIndex {
		t.Fatalf(
			"failed structured terminal order = %v, want agent_end before settled",
			eventTypes(events),
		)
	}
}

func TestStructuredPrintModeUsesTheDurableRuntime(t *testing.T) {
	ctx := t.Context()
	path := filepath.Join(t.TempDir(), "events.db")
	store, err := session.NewSQLiteStore(path, "events-acceptance")
	if err != nil {
		t.Fatalf("open events store: %v", err)
	}
	sess := session.NewSession(store, 64)
	runner := agent.NewController(agent.ControllerConfig{
		Session: sess,
		Store:   store,
		Durable: store,
		Model:   llm.Model{ID: "fake-model", Provider: "fake"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &printAcceptanceStream{chunks: []*llm.Chunk{
				{Content: "structured", StopReason: "stop"},
			}}, nil
		},
	})
	defer func() {
		if err := runner.Close(); err != nil {
			t.Errorf("close runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("close store: %v", err)
		}
	}()

	var out bytes.Buffer
	if err := runPrintModeWithWriter(ctx, &out, runner, "emit events", "events"); err != nil {
		t.Fatalf("run structured print mode: %v", err)
	}

	var events []structuredPrintEvent
	decoder := json.NewDecoder(&out)
	for {
		var event structuredPrintEvent
		err := decoder.Decode(&event)
		if err == io.EOF {
			break
		}
		if err != nil {
			t.Fatalf("decode structured output %q: %v", out.String(), err)
		}
		events = append(events, event)
	}
	if len(events) < 3 {
		t.Fatalf("structured event count = %d, want lifecycle and result: %s", len(events), out.String())
	}
	wantTypes := []string{"turn_start", "message_update", "turn_end", "result"}
	gotTypes := make([]string, 0, len(events))
	for index, event := range events {
		if event.Schema != printEventSchema || event.Index != uint64(index+1) {
			t.Fatalf("event[%d] = %#v, want schema %q and local index %d", index, event, printEventSchema, index+1)
		}
		gotTypes = append(gotTypes, event.Type)
	}
	for _, want := range wantTypes {
		if !slices.Contains(gotTypes, want) {
			t.Fatalf("structured event types = %v, missing %q", gotTypes, want)
		}
	}
	last := events[len(events)-1]
	if last.Type != "result" {
		t.Fatalf("last structured event = %#v, want result", last)
	}
	var result printResult
	data, err := json.Marshal(last.Data)
	if err != nil {
		t.Fatalf("marshal structured result: %v", err)
	}
	if err := json.Unmarshal(data, &result); err != nil {
		t.Fatalf("decode structured result: %v", err)
	}
	leafID := store.GetLeafID()
	if leafID == "" {
		t.Fatal("structured print turn did not materialize a session leaf")
	}
	stableID := runner.SessionID()
	if result.Response != "structured" || result.SessionID != stableID || result.LeafID != leafID {
		t.Fatalf("structured result = %#v, want stable ID %q and resumable leaf %q", result, stableID, leafID)
	}
	if err := store.ResumeSession(ctx, result.SessionID); err != nil {
		t.Fatalf("structured result session_id is not resumable: %v", err)
	}
	if got := store.GetLeafID(); got != leafID {
		t.Fatalf("resumed stable result leaf = %q, want %q", got, leafID)
	}
}

type printAcceptanceStream struct {
	chunks []*llm.Chunk
	index  int
}

func (s *printAcceptanceStream) Next() (*llm.Chunk, bool) {
	if s.index >= len(s.chunks) {
		return nil, false
	}
	chunk := s.chunks[s.index]
	s.index++
	return chunk, true
}

func (s *printAcceptanceStream) Err() error   { return nil }
func (s *printAcceptanceStream) Close() error { return nil }
