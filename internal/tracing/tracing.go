// Package tracing provides OpenTelemetry instrumentation for ion agents.
//
// Span hierarchy per session/turn:
//
//	ion.session
//	└── ion.turn
//	    ├── ion.context   (context pipeline build)
//	    ├── gen_ai.chat     (provider.Generate)
//	    └── ion.tool.{name}  (tool executions, one per call)
//
// Typical usage:
//
//	provider := tracing.WrapProvider(baseProvider)
//	reg.Register(tracing.WrapTool(myTool))
//
//	ctx, span := tracing.StartSession(ctx, agentID, sessID, model)
//	defer tracing.EndSession(span, err)
//	ctx, span := tracing.StartTurn(ctx, agentID, sessID, model)
//	defer tracing.EndTurn(span, err)
//	result, err := agent.Turn(ctx, sess)
package tracing

import (
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/trace"
)

const tracerName = "github.com/nijaru/ion"

// Tracer returns the ion tracer.
func Tracer() trace.Tracer {
	return otel.Tracer(tracerName)
}
