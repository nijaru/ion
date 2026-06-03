package tracing

import (
	"context"
	"iter"
	"strings"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/trace"
	"github.com/nijaru/ion/internal/approval"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/tool"
)

type wrappedTool struct {
	inner tool.Tool
}

func (*wrappedTool) tracingWrapped() {}

// WrapTool returns a Tool that records a "canto.tool.{name}" child span on
// every Execute call. If the tool is a StreamingUpdateTool or StreamingTool,
// the returned tool will also implement the same streaming surface and
// instrument it. If the tool is a ContentTool, the returned tool preserves
// ExecuteContent.
func WrapTool(t tool.Tool) tool.Tool {
	if _, ok := t.(interface{ tracingWrapped() }); ok {
		return t
	}
	w := wrappedTool{inner: t}
	if st, ok := t.(tool.StreamingUpdateTool); ok {
		return &wrappedStreamingUpdateTool{wrappedTool: w, innerStreamingUpdates: st}
	}
	if st, ok := t.(tool.StreamingTool); ok {
		return &wrappedStreamingTool{wrappedTool: w, innerStreaming: st}
	}
	if ct, ok := t.(tool.ContentTool); ok {
		return &wrappedContentTool{wrappedTool: w, innerContent: ct}
	}
	return &w
}

func (w *wrappedTool) Spec() llm.Spec { return w.inner.Spec() }

func (w *wrappedTool) Metadata() tool.Metadata {
	if mt, ok := w.inner.(tool.MetadataTool); ok {
		return mt.Metadata()
	}
	return tool.Metadata{}
}

func (w *wrappedTool) ApprovalRequirement(args string) (approval.Requirement, bool, error) {
	if at, ok := w.inner.(approval.RequirementProvider); ok {
		return at.ApprovalRequirement(args)
	}
	return approval.Requirement{}, false, nil
}

func (w *wrappedTool) Execute(ctx context.Context, args string) (string, error) {
	name := w.inner.Spec().Name
	ctx, span := Tracer().Start(
		ctx, "canto.tool."+name,
		trace.WithAttributes(attribute.String("canto.tool.name", name)),
	)
	defer span.End()

	out, err := w.inner.Execute(ctx, args)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
	}
	return out, err
}

type wrappedContentTool struct {
	wrappedTool
	innerContent tool.ContentTool
}

func (*wrappedContentTool) tracingWrapped() {}

func (w *wrappedContentTool) ExecuteContent(
	ctx context.Context,
	args string,
) ([]llm.ContentPart, error) {
	name := w.inner.Spec().Name
	ctx, span := Tracer().Start(
		ctx,
		"canto.tool."+name,
		trace.WithAttributes(
			attribute.String("canto.tool.name", name),
			attribute.Bool("canto.tool.content_parts", true),
		),
	)
	defer span.End()

	parts, err := w.innerContent.ExecuteContent(ctx, args)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return parts, err
	}
	span.SetAttributes(
		attribute.Int("canto.tool.parts", len(parts)),
		attribute.Int("canto.tool.output_len", len(llm.Message{Parts: parts}.TextContent())),
	)
	return parts, nil
}

type wrappedStreamingTool struct {
	wrappedTool
	innerStreaming tool.StreamingTool
}

func (*wrappedStreamingTool) tracingWrapped() {}

func (w *wrappedStreamingTool) ExecuteStreaming(
	ctx context.Context,
	args string,
) iter.Seq2[string, error] {
	name := w.inner.Spec().Name

	return func(yield func(string, error) bool) {
		ctx, span := Tracer().Start(
			ctx, "canto.tool."+name,
			trace.WithAttributes(
				attribute.String("canto.tool.name", name),
				attribute.Bool("canto.tool.streaming", true),
			),
		)
		streamCtx, cancel := context.WithCancel(ctx)
		defer cancel()
		defer span.End()
		var buf strings.Builder
		for delta, err := range w.innerStreaming.ExecuteStreaming(streamCtx, args) {
			if err != nil {
				span.RecordError(err)
				span.SetStatus(codes.Error, err.Error())
				if !yield("", err) {
					cancel()
					return
				}
				return
			}
			buf.WriteString(delta)
			if !yield(delta, nil) {
				cancel()
				return
			}
		}
		span.SetAttributes(attribute.Int("canto.tool.output_len", buf.Len()))
	}
}

type wrappedStreamingUpdateTool struct {
	wrappedTool
	innerStreamingUpdates tool.StreamingUpdateTool
}

func (*wrappedStreamingUpdateTool) tracingWrapped() {}

func (w *wrappedStreamingUpdateTool) ExecuteStreamingUpdates(
	ctx context.Context,
	args string,
) iter.Seq2[tool.StreamUpdate, error] {
	name := w.inner.Spec().Name

	return func(yield func(tool.StreamUpdate, error) bool) {
		ctx, span := Tracer().Start(
			ctx, "canto.tool."+name,
			trace.WithAttributes(
				attribute.String("canto.tool.name", name),
				attribute.Bool("canto.tool.streaming", true),
				attribute.Bool("canto.tool.streaming_updates", true),
			),
		)
		streamCtx, cancel := context.WithCancel(ctx)
		defer cancel()
		defer span.End()
		var buf strings.Builder
		for update, err := range w.innerStreamingUpdates.ExecuteStreamingUpdates(streamCtx, args) {
			if err != nil {
				span.RecordError(err)
				span.SetStatus(codes.Error, err.Error())
				if !yield(tool.StreamUpdate{}, err) {
					cancel()
					return
				}
				return
			}
			if update.Snapshot {
				buf.Reset()
			}
			buf.WriteString(update.Text)
			if !yield(update, nil) {
				cancel()
				return
			}
		}
		span.SetAttributes(attribute.Int("canto.tool.output_len", buf.Len()))
	}
}

func (w *wrappedStreamingUpdateTool) ExecuteStreaming(
	ctx context.Context,
	args string,
) iter.Seq2[string, error] {
	return func(yield func(string, error) bool) {
		for update, err := range w.ExecuteStreamingUpdates(ctx, args) {
			if err != nil {
				if !yield("", err) {
					return
				}
				return
			}
			if !yield(update.Text, nil) {
				return
			}
		}
	}
}
