package approval

import (
	"context"

	"github.com/nijaru/ion/internal/llm"
	prompt "github.com/nijaru/ion/internal/prompt"
	"github.com/nijaru/ion/internal/storage/session"
)

// CircuitBreakerGuard injects a warning into the prompt if the approval gate
// is in a tripped state.
type CircuitBreakerGuard struct {
	Gate *Gate
}

// NewCircuitBreakerGuard creates a prompt guard for a gate's circuit breaker.
func NewCircuitBreakerGuard(gate *Gate) *CircuitBreakerGuard {
	return &CircuitBreakerGuard{Gate: gate}
}

func (g *CircuitBreakerGuard) ApplyRequest(
	ctx context.Context,
	provider llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	if g.Gate == nil || !g.Gate.IsTripped() {
		return nil
	}

	hint := "Notice: Automated tool approvals are currently disabled due to repeated safety denials. " +
		"Every subsequent tool call will require manual human approval until the agent demonstrates safe behavior."

	return prompt.Instructions(hint).ApplyRequest(ctx, provider, model, sess, req)
}
