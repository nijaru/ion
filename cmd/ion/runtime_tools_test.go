package main

import (
	"context"
	"encoding/json"
	"errors"
	"iter"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

type runtimeTestTool struct {
	output string
	err    error
}

func (t runtimeTestTool) Spec() llm.Spec { return llm.Spec{Name: "test"} }
func (t runtimeTestTool) Execute(context.Context, string) (string, error) {
	return t.output, t.err
}

type runtimeContentTool struct{}

func (runtimeContentTool) Spec() llm.Spec                                  { return llm.Spec{Name: "content"} }
func (runtimeContentTool) Execute(context.Context, string) (string, error) { return "", nil }
func (runtimeContentTool) ExecuteContent(context.Context, string) ([]llm.ContentPart, error) {
	return []llm.ContentPart{
		llm.TextPart("before"),
		llm.ImagePart("image/png", "aW1hZ2U="),
	}, nil
}

type runtimeStreamingTool struct{}

func (runtimeStreamingTool) Spec() llm.Spec                                  { return llm.Spec{Name: "stream"} }
func (runtimeStreamingTool) Execute(context.Context, string) (string, error) { return "", nil }
func (runtimeStreamingTool) ExecuteStreamingUpdates(context.Context, string) iter.Seq2[tool.StreamUpdate, error] {
	return func(yield func(tool.StreamUpdate, error) bool) {
		yield(tool.StreamUpdate{Text: "first"}, nil)
		yield(tool.StreamUpdate{Text: " final", Snapshot: true}, nil)
	}
}

func runtimeEntry(t tool.Tool) tool.ToolEntry {
	return tool.ToolEntry{Tool: t, Spec: t.Spec()}
}

func TestExecutionModeForToolMetadataFailsClosed(t *testing.T) {
	if got := executionModeFor(tool.Metadata{Concurrency: tool.Parallel}); got != agent.ExecParallel {
		t.Fatalf("parallel mode = %q, want parallel", got)
	}
	if got := executionModeFor(tool.Metadata{Concurrency: tool.Serialized}); got != agent.ExecSequential {
		t.Fatalf("serialized mode = %q, want sequential", got)
	}
	if got := executionModeFor(tool.Metadata{}); got != agent.ExecSequential {
		t.Fatalf("unknown mode = %q, want sequential", got)
	}
}

func TestExecuteRegisteredToolPreservesOutputOnError(t *testing.T) {
	result := executeRegisteredTool(
		context.Background(),
		runtimeEntry(runtimeTestTool{output: "stdout", err: errors.New("exit status 1")}),
		"call-1",
		json.RawMessage(`{}`),
		nil,
	)
	if !result.IsError || session.EntryText(&session.MessageEntry{Message: &result}) != "stdoutexit status 1" {
		t.Fatalf("result = %#v, want output and error", result)
	}
}

func TestExecuteRegisteredToolPreservesContentParts(t *testing.T) {
	result := executeRegisteredTool(context.Background(), runtimeEntry(runtimeContentTool{}), "call-2", nil, nil)
	if len(result.Content) != 2 {
		t.Fatalf("content parts = %#v, want text and image", result.Content)
	}
	if _, ok := result.Content[1].(session.ImageContent); !ok {
		t.Fatalf("content[1] = %T, want ImageContent", result.Content[1])
	}
}

func TestExecuteRegisteredToolStreamsUpdatesAndReturnsFinalSnapshot(t *testing.T) {
	var updates []string
	result := executeRegisteredTool(context.Background(), runtimeEntry(runtimeStreamingTool{}), "call-3", nil, func(partial session.ToolPartial) {
		updates = append(updates, partial.(string))
	})
	if session.EntryText(&session.MessageEntry{Message: &result}) != " final" {
		t.Fatalf("stream result = %#v, want final snapshot", result)
	}
	if len(updates) != 2 || updates[0] != "first" || updates[1] != " final" {
		t.Fatalf("updates = %#v, want both text updates", updates)
	}
}

func TestContextWithToolSignalCancelsToolContext(t *testing.T) {
	signal := make(chan struct{})
	toolCtx, cancel := contextWithToolSignal(context.Background(), signal)
	defer cancel()
	close(signal)
	select {
	case <-toolCtx.Done():
	case <-time.After(time.Second):
		t.Fatal("tool context did not cancel")
	}
}
