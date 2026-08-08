package main

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"path/filepath"
	"slices"
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
	info, err := store.GetSessionInfo(ctx, leafID)
	if err != nil {
		t.Fatalf("load print session catalog by leaf: %v", err)
	}
	if info.ID() != leafID || info.LastPreview != "say hello" {
		t.Fatalf("print session catalog = %#v, want leaf %q and prompt preview", info, leafID)
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
	if result.Response != "structured" || result.SessionID != leafID {
		t.Fatalf("structured result = %#v, want resumable leaf %q", result, leafID)
	}
	if err := store.ResumeSession(ctx, result.SessionID); err != nil {
		t.Fatalf("structured result session_id is not resumable: %v", err)
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
