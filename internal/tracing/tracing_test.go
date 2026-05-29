package tracing_test

import (
	"context"
	"iter"
	"testing"
	"time"

	"go.opentelemetry.io/otel"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/tool"
	"github.com/nijaru/ion/internal/tracing"
	xtest "github.com/nijaru/ion/internal/cantotesting"
)

type cancelAwareStreamingTool struct {
	canceled chan struct{}
}

func (t *cancelAwareStreamingTool) Spec() llm.Spec {
	return llm.Spec{Name: "streaming_tool", Description: "stream"}
}

func (t *cancelAwareStreamingTool) Execute(context.Context, string) (string, error) {
	return "", nil
}

func (t *cancelAwareStreamingTool) ExecuteStreaming(
	ctx context.Context,
	_ string,
) iter.Seq2[string, error] {
	return func(yield func(string, error) bool) {
		if yield("first", nil) {
			return
		}
		select {
		case <-ctx.Done():
			close(t.canceled)
		case <-time.After(time.Second):
		}
	}
}

func setupTracer(t *testing.T) *tracetest.SpanRecorder {
	t.Helper()
	rec := tracetest.NewSpanRecorder()
	tp := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(rec))
	otel.SetTracerProvider(tp)
	t.Cleanup(func() { tp.Shutdown(t.Context()) }) //nolint
	return rec
}

func TestStartSessionAndTurnHierarchy(t *testing.T) {
	rec := setupTracer(t)

	ctx, sessionSpan := tracing.StartSession(t.Context(), "agent1", "sess1", "gpt-4o")
	ctx, turnSpan := tracing.StartTurn(ctx, "agent1", "sess1", "gpt-4o")
	tracing.EndTurn(turnSpan, nil)
	tracing.EndSession(sessionSpan, nil)

	spans := rec.Ended()
	if len(spans) != 2 {
		t.Fatalf("expected 2 spans, got %d", len(spans))
	}
	if spans[0].Name() != "canto.turn" && spans[1].Name() != "canto.turn" {
		t.Fatalf("expected a canto.turn span, got %#v", spans)
	}
	if spans[0].Name() != "canto.session" && spans[1].Name() != "canto.session" {
		t.Fatalf("expected a canto.session span, got %#v", spans)
	}
	_ = ctx
}

func TestStartContext(t *testing.T) {
	rec := setupTracer(t)

	ctx, span := tracing.StartContext(t.Context(), "agent1", "sess1", "gpt-4o")
	tracing.EndContext(span, nil)

	spans := rec.Ended()
	if len(spans) != 1 {
		t.Fatalf("expected 1 span, got %d", len(spans))
	}
	if spans[0].Name() != "canto.context" {
		t.Fatalf("expected canto.context span, got %q", spans[0].Name())
	}
	attrs := spans[0].Attributes()
	found := map[string]string{}
	for _, attr := range attrs {
		found[string(attr.Key)] = attr.Value.AsString()
	}
	if found["canto.agent_id"] != "agent1" || found["canto.session_id"] != "sess1" ||
		found["gen_ai.request.model"] != "gpt-4o" {
		t.Fatalf("unexpected context span attrs: %#v", found)
	}
	_ = ctx
}

func TestWrapProvider_RecordsGenAIChatSpan(t *testing.T) {
	rec := setupTracer(t)

	mock := xtest.NewFauxProvider("test", xtest.Step{Content: "hello"})
	p := tracing.WrapProvider(mock)

	ctx, sessionSpan := tracing.StartSession(t.Context(), "a", "s", "m")
	ctx, turnSpan := tracing.StartTurn(ctx, "a", "s", "m")

	_, err := p.Generate(ctx, &llm.Request{Model: "m"})
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	tracing.EndTurn(turnSpan, nil)
	tracing.EndSession(sessionSpan, nil)

	var names []string
	for _, s := range rec.Ended() {
		names = append(names, s.Name())
	}
	found := func(name string) bool {
		for _, n := range names {
			if n == name {
				return true
			}
		}
		return false
	}
	if !found("gen_ai.chat") {
		t.Errorf("gen_ai.chat span not recorded; got: %v", names)
	}
	if !found("canto.turn") || !found("canto.session") {
		t.Errorf("expected session+turn spans not recorded; got: %v", names)
	}
}

func TestWrapTool_RecordsToolSpan(t *testing.T) {
	rec := setupTracer(t)

	inner := tool.Func(
		"my_tool", "desc", nil,
		func(ctx context.Context, args string) (string, error) { return "ok", nil },
	)
	wrapped := tracing.WrapTool(inner)

	ctx, sessionSpan := tracing.StartSession(t.Context(), "a", "s", "m")
	ctx, turnSpan := tracing.StartTurn(ctx, "a", "s", "m")
	out, err := wrapped.Execute(ctx, "{}")
	tracing.EndTurn(turnSpan, err)
	tracing.EndSession(sessionSpan, nil)

	if err != nil || out != "ok" {
		t.Fatalf("Execute: out=%q err=%v", out, err)
	}

	var names []string
	for _, s := range rec.Ended() {
		names = append(names, s.Name())
	}
	foundTool := false
	for _, n := range names {
		if n == "canto.tool.my_tool" {
			foundTool = true
		}
	}
	if !foundTool {
		t.Errorf("canto.tool.my_tool span not recorded; got: %v", names)
	}
}

func TestWrapProviderIdempotent(t *testing.T) {
	rec := setupTracer(t)

	mock := xtest.NewFauxProvider("test", xtest.Step{Content: "hello"})
	p := tracing.WrapProvider(tracing.WrapProvider(mock))

	ctx, sessionSpan := tracing.StartSession(t.Context(), "a", "s", "m")
	ctx, turnSpan := tracing.StartTurn(ctx, "a", "s", "m")

	_, err := p.Generate(ctx, &llm.Request{Model: "m"})
	tracing.EndTurn(turnSpan, err)
	tracing.EndSession(sessionSpan, nil)
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}

	var chatSpans int
	for _, s := range rec.Ended() {
		if s.Name() == "gen_ai.chat" {
			chatSpans++
		}
	}
	if chatSpans != 1 {
		t.Fatalf("expected one gen_ai.chat span, got %d", chatSpans)
	}
}

func TestWrapToolIdempotent(t *testing.T) {
	rec := setupTracer(t)

	inner := tool.Func(
		"my_tool", "desc", nil,
		func(ctx context.Context, args string) (string, error) { return "ok", nil },
	)
	wrapped := tracing.WrapTool(tracing.WrapTool(inner))

	ctx, sessionSpan := tracing.StartSession(t.Context(), "a", "s", "m")
	ctx, turnSpan := tracing.StartTurn(ctx, "a", "s", "m")
	out, err := wrapped.Execute(ctx, "{}")
	tracing.EndTurn(turnSpan, err)
	tracing.EndSession(sessionSpan, nil)

	if err != nil || out != "ok" {
		t.Fatalf("Execute: out=%q err=%v", out, err)
	}

	var toolSpans int
	for _, s := range rec.Ended() {
		if s.Name() == "canto.tool.my_tool" {
			toolSpans++
		}
	}
	if toolSpans != 1 {
		t.Fatalf("expected one tool span, got %d", toolSpans)
	}
}

func TestWrapStreamingToolCancelsInnerWhenConsumerStops(t *testing.T) {
	setupTracer(t)

	inner := &cancelAwareStreamingTool{canceled: make(chan struct{})}
	wrapped := tracing.WrapTool(inner).(tool.StreamingTool)

	for delta, err := range wrapped.ExecuteStreaming(t.Context(), "{}") {
		if err != nil {
			t.Fatalf("ExecuteStreaming: %v", err)
		}
		if delta != "first" {
			t.Fatalf("delta = %q, want first", delta)
		}
		break
	}

	select {
	case <-inner.canceled:
	case <-time.After(time.Second):
		t.Fatal("inner streaming tool was not canceled")
	}
}

func TestWrapStreamingToolDoesNotStartSpanBeforeConsumption(t *testing.T) {
	rec := setupTracer(t)

	inner := &cancelAwareStreamingTool{canceled: make(chan struct{})}
	wrapped := tracing.WrapTool(inner).(tool.StreamingTool)
	_ = wrapped.ExecuteStreaming(t.Context(), "{}")

	if got := len(rec.Started()); got != 0 {
		t.Fatalf("started spans = %d, want 0 before stream consumption", got)
	}
}
