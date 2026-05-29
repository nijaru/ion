// Package tracing provides OpenTelemetry instrumentation for canto agents.
//
// Span hierarchy per session/turn:
//
//	canto.session
//	└── canto.turn
//	    ├── canto.context   (context pipeline build)
//	    ├── gen_ai.chat     (provider.Generate)
//	    └── canto.tool.{name}  (tool executions, one per call)
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

const tracerName = "github.com/nijaru/canto"

// Tracer returns the canto tracer.
func Tracer() trace.Tracer {
	return otel.Tracer(tracerName)
}
