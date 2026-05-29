package tracing

import (
	"context"
	"encoding/json"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/trace"

	"github.com/nijaru/ion/internal/llm"
)

// WrapOptions configures the instrumentation behavior.
type WrapOptions struct {
	RecordMessages bool
}

// WrapOption is a functional option for WrapProvider and WrapTool.
type WrapOption func(*WrapOptions)

// WithRecordMessages enables recording of the raw prompt and completion messages
// in the telemetry spans (as gen_ai.input.messages and gen_ai.output.messages).
// By default, messages are dropped to prevent PII leakage.
func WithRecordMessages(record bool) WrapOption {
	return func(o *WrapOptions) {
		o.RecordMessages = record
	}
}

type wrappedProvider struct {
	inner          llm.Provider
	recordMessages bool
}

func (*wrappedProvider) tracingWrapped() {}

// WrapProvider returns a Provider that records a "gen_ai.chat" child span on
// every Generate call.
func WrapProvider(p llm.Provider, opts ...WrapOption) llm.Provider {
	if _, ok := p.(interface{ tracingWrapped() }); ok {
		return p
	}
	var options WrapOptions
	for _, opt := range opts {
		opt(&options)
	}
	return &wrappedProvider{
		inner:          p,
		recordMessages: options.RecordMessages,
	}
}

func (w *wrappedProvider) ID() string { return w.inner.ID() }

func (w *wrappedProvider) Generate(
	ctx context.Context,
	req *llm.Request,
) (*llm.Response, error) {
	if err := llm.ValidateRequest(req); err != nil {
		return nil, err
	}

	attrs := []attribute.KeyValue{
		attribute.String("gen_ai.request.model", req.Model),
		attribute.Int("gen_ai.request.message_count", len(req.Messages)),
	}

	if w.recordMessages {
		if b, err := json.Marshal(req.Messages); err == nil {
			attrs = append(attrs, attribute.String("gen_ai.input.messages", string(b)))
		}
	}

	ctx, span := Tracer().Start(ctx, "gen_ai.chat", trace.WithAttributes(attrs...))
	defer span.End()

	resp, err := w.inner.Generate(ctx, req)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, err
	}

	respAttrs := []attribute.KeyValue{
		attribute.Int("gen_ai.usage.input_tokens", resp.Usage.InputTokens),
		attribute.Int("gen_ai.usage.output_tokens", resp.Usage.OutputTokens),
		attribute.Int("gen_ai.usage.cache_read.input_tokens", resp.Usage.CacheReadTokens),
		attribute.Int("gen_ai.usage.cache_creation.input_tokens", resp.Usage.CacheCreationTokens),
		attribute.Int("gen_ai.response.tool_call_count", len(resp.Calls)),
	}
	if w.recordMessages {
		if b, err := json.Marshal(resp); err == nil {
			respAttrs = append(respAttrs, attribute.String("gen_ai.output.messages", string(b)))
		}
	}

	span.SetAttributes(respAttrs...)
	return resp, nil
}

func (w *wrappedProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	if err := llm.ValidateRequest(req); err != nil {
		return nil, err
	}

	attrs := []attribute.KeyValue{
		attribute.String("gen_ai.request.model", req.Model),
		attribute.Int("gen_ai.request.message_count", len(req.Messages)),
		attribute.Bool("gen_ai.request.stream", true),
	}

	if w.recordMessages {
		if b, err := json.Marshal(req.Messages); err == nil {
			attrs = append(attrs, attribute.String("gen_ai.input.messages", string(b)))
		}
	}

	ctx, span := Tracer().Start(ctx, "gen_ai.chat", trace.WithAttributes(attrs...))

	stream, err := w.inner.Stream(ctx, req)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		span.End()
		return nil, err
	}

	return &wrappedStream{inner: stream, span: span, recordMessages: w.recordMessages}, nil
}

func (w *wrappedProvider) Models(ctx context.Context) ([]llm.Model, error) {
	return w.inner.Models(ctx)
}

func (w *wrappedProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	return w.inner.CountTokens(ctx, model, messages)
}

func (w *wrappedProvider) Cost(ctx context.Context, model string, usage llm.Usage) float64 {
	return w.inner.Cost(ctx, model, usage)
}

func (w *wrappedProvider) Capabilities(model string) llm.Capabilities {
	return w.inner.Capabilities(model)
}

func (w *wrappedProvider) IsTransient(err error) bool {
	return w.inner.IsTransient(err)
}

func (w *wrappedProvider) IsContextOverflow(err error) bool {
	return w.inner.IsContextOverflow(err)
}

type wrappedStream struct {
	inner          llm.Stream
	span           trace.Span
	usage          llm.Usage
	recordMessages bool
	chunks         []llm.Chunk
}

func (w *wrappedStream) Next() (*llm.Chunk, bool) {
	chunk, ok := w.inner.Next()
	if ok {
		if chunk.Usage != nil {
			w.usage.InputTokens += chunk.Usage.InputTokens
			w.usage.OutputTokens += chunk.Usage.OutputTokens
			w.usage.TotalTokens += chunk.Usage.TotalTokens
			w.usage.CacheReadTokens += chunk.Usage.CacheReadTokens
			w.usage.CacheCreationTokens += chunk.Usage.CacheCreationTokens
		}

		if w.recordMessages {
			w.chunks = append(w.chunks, *chunk)
		}
	}
	return chunk, ok
}

func (w *wrappedStream) Err() error { return w.inner.Err() }

func (w *wrappedStream) Close() error {
	err := w.inner.Close()
	if err != nil {
		w.span.RecordError(err)
		w.span.SetStatus(codes.Error, err.Error())
	} else if serr := w.inner.Err(); serr != nil {
		w.span.RecordError(serr)
		w.span.SetStatus(codes.Error, serr.Error())
	}
	attrs := []attribute.KeyValue{
		attribute.Int("gen_ai.usage.input_tokens", w.usage.InputTokens),
		attribute.Int("gen_ai.usage.output_tokens", w.usage.OutputTokens),
		attribute.Int("gen_ai.usage.cache_read.input_tokens", w.usage.CacheReadTokens),
		attribute.Int("gen_ai.usage.cache_creation.input_tokens", w.usage.CacheCreationTokens),
	}

	if w.recordMessages && len(w.chunks) > 0 {
		if b, err := json.Marshal(w.chunks); err == nil {
			attrs = append(attrs, attribute.String("gen_ai.output.messages", string(b)))
		}
	}
	w.span.SetAttributes(attrs...)
	w.span.End()
	return err
}
