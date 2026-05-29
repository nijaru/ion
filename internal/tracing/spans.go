package tracing

import (
	"context"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/trace"
)

// StartSession starts a "canto.session" root span for a session execution.
func StartSession(
	ctx context.Context,
	agentID, sessionID, model string,
) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.session",
		trace.WithAttributes(
			attribute.String("canto.agent_id", agentID),
			attribute.String("canto.session_id", sessionID),
			attribute.String("gen_ai.request.model", model),
		),
	)
}

// EndSession ends a session span, setting the error status if err is non-nil.
func EndSession(span trace.Span, err error) {
	endSpan(span, err)
}

// StartTurn starts a "canto.turn" child span and returns the derived context
// and span. The caller must call span.End() when the turn is complete.
func StartTurn(
	ctx context.Context,
	agentID, sessionID, model string,
) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.turn",
		trace.WithAttributes(
			attribute.String("canto.agent_id", agentID),
			attribute.String("canto.session_id", sessionID),
			attribute.String("gen_ai.request.model", model),
		),
	)
}

// EndTurn ends a turn span, setting the error status if err is non-nil.
func EndTurn(span trace.Span, err error) {
	endSpan(span, err)
}

// StartGraph starts a "canto.graph" child span for a graph execution.
func StartGraph(ctx context.Context, graphID, sessionID string) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.graph",
		trace.WithAttributes(
			attribute.String("canto.graph_id", graphID),
			attribute.String("canto.session_id", sessionID),
		),
	)
}

// StartNode starts a "canto.graph.node" child span for a node in a graph.
func StartNode(ctx context.Context, nodeID string) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.graph.node",
		trace.WithAttributes(attribute.String("canto.node_id", nodeID)),
	)
}

// StartSwarm starts a "canto.swarm" child span for a swarm execution.
func StartSwarm(ctx context.Context, sessionID string) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.swarm",
		trace.WithAttributes(attribute.String("canto.session_id", sessionID)),
	)
}

// StartSwarmRound starts a "canto.swarm.round" child span for a single round in a swarm.
func StartSwarmRound(ctx context.Context, round int) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.swarm.round",
		trace.WithAttributes(attribute.Int("canto.swarm.round", round)),
	)
}

// StartAgent starts a "canto.agent" child span.
func StartAgent(ctx context.Context, agentID string) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.agent",
		trace.WithAttributes(attribute.String("canto.agent_id", agentID)),
	)
}

// StartContext starts a "canto.context" child span for the context-pipeline
// build phase. Call this immediately before builder.Build.
func StartContext(
	ctx context.Context,
	agentID, sessionID, model string,
) (context.Context, trace.Span) {
	return Tracer().Start(
		ctx, "canto.context",
		trace.WithAttributes(
			attribute.String("canto.agent_id", agentID),
			attribute.String("canto.session_id", sessionID),
			attribute.String("gen_ai.request.model", model),
		),
	)
}

// EndContext ends a context-build span, setting the error status if err is non-nil.
func EndContext(span trace.Span, err error) {
	endSpan(span, err)
}

func endSpan(span trace.Span, err error) {
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
	}
	span.End()
}
