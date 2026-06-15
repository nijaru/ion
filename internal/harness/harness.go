// Package harness provides the composition layer wrapping the agent loop.
//
// The harness owns:
//   - Session management (tree store, compaction, branching)
//   - Hook dispatch (20+ lifecycle events)
//   - Model routing (dynamic provider/model selection)
//   - Extension integration (stdio JSON-RPC plugins)
//
// It does NOT own:
//   - Turn sequencing (that's the agent loop)
//   - Tool execution (that's the agent loop)
//   - LLM streaming (that's the agent loop)
package harness

import (
	"context"
	"fmt"
	"sync"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

// Harness is the composition layer wrapping the agent loop.
type Harness struct {
	agent      *agent.Agent
	hooks      *HookDispatcher
	mu         sync.RWMutex
}

// Config holds the harness configuration.
type Config struct {
	// Agent is the agent to wrap.
	Agent *agent.Agent
	// Hooks are optional lifecycle hooks.
	Hooks *HookDispatcher
}

// New creates a new harness.
func New(cfg Config) *Harness {
	if cfg.Hooks == nil {
		cfg.Hooks = NewHookDispatcher()
	}
	return &Harness{
		agent: cfg.Agent,
		hooks: cfg.Hooks,
	}
}

// Agent returns the underlying agent.
func (h *Harness) Agent() *agent.Agent {
	return h.agent
}

// Hooks returns the hook dispatcher.
func (h *Harness) Hooks() *HookDispatcher {
	return h.hooks
}

// Run starts the agent loop with the given prompt messages.
// It dispatches lifecycle hooks around the agent run.
func (h *Harness) Run(ctx context.Context, prompts []agent.AgentMessage) ([]agent.AgentMessage, error) {
	// Dispatch before_agent_start hook
	result, err := h.hooks.Dispatch(ctx, HookEvent{
		Type: BeforeAgentStart,
		Payload: BeforeAgentStartPayload{
			Prompts: prompts,
		},
	})
	if err != nil {
		return nil, fmt.Errorf("before_agent_start hook: %w", err)
	}
	if result.Abort {
		return nil, fmt.Errorf("aborted by before_agent_start hook: %s", result.Reason)
	}

	// Run the agent
	messages, err := h.agent.Run(ctx, prompts)

	// Dispatch after_agent_start hook
	h.hooks.Dispatch(ctx, HookEvent{
		Type: AfterAgentRun,
		Payload: AfterAgentRunPayload{
			Messages: messages,
			Error:    err,
		},
	})

	return messages, err
}

// Continue continues the agent loop without adding new messages.
func (h *Harness) Continue(ctx context.Context) ([]agent.AgentMessage, error) {
	// Dispatch before_agent_start hook
	result, err := h.hooks.Dispatch(ctx, HookEvent{
		Type: BeforeAgentStart,
		Payload: BeforeAgentStartPayload{},
	})
	if err != nil {
		return nil, fmt.Errorf("before_agent_start hook: %w", err)
	}
	if result.Abort {
		return nil, fmt.Errorf("aborted by before_agent_start hook: %s", result.Reason)
	}

	// Continue the agent
	messages, err := h.agent.Continue(ctx)

	// Dispatch after_agent_run hook
	h.hooks.Dispatch(ctx, HookEvent{
		Type: AfterAgentRun,
		Payload: AfterAgentRunPayload{
			Messages: messages,
			Error:    err,
		},
	})

	return messages, err
}

// CancelTurn interrupts the current turn.
func (h *Harness) CancelTurn(ctx context.Context) error {
	return h.agent.CancelTurn(ctx)
}

// Reset clears the agent state.
func (h *Harness) Reset() {
	h.agent.Reset()
}

// Close closes the harness and the underlying agent.
func (h *Harness) Close() error {
	return h.agent.Close()
}

// Events returns the agent's event channel.
func (h *Harness) Events() <-chan session.AgentEvent {
	return h.agent.Events()
}

// ID returns the harness ID.
func (h *Harness) ID() string {
	return h.agent.ID()
}

// SteerTurn sends a steering message to the agent.
func (h *Harness) SteerTurn(ctx context.Context, text string) (session.SteeringResult, error) {
	return h.agent.SteerTurn(ctx, text)
}

// FollowUpTurn sends a follow-up message to the agent.
func (h *Harness) FollowUpTurn(ctx context.Context, text string) (session.QueuedInputResult, error) {
	return h.agent.FollowUpTurn(ctx, text)
}
