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
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Harness is the composition layer wrapping the agent loop.
type Harness struct {
	agent      *agent.Agent
	hooks      *HookDispatcher
	router     *ModelRouter
	extensions *ExtensionManager
	mu         sync.RWMutex
}

// Config holds the harness configuration.
type Config struct {
	// Agent is the agent to wrap.
	Agent *agent.Agent
	// Hooks are optional lifecycle hooks.
	Hooks *HookDispatcher
	// Router is optional model router.
	Router *ModelRouter
	// Extensions is optional extension manager.
	Extensions *ExtensionManager
}

// New creates a new harness.
func New(cfg Config) *Harness {
	if cfg.Hooks == nil {
		cfg.Hooks = NewHookDispatcher()
	}
	if cfg.Router == nil {
		cfg.Router = NewModelRouter()
	}
	if cfg.Extensions == nil {
		cfg.Extensions = NewExtensionManager()
	}
	return &Harness{
		agent:      cfg.Agent,
		hooks:      cfg.Hooks,
		router:     cfg.Router,
		extensions: cfg.Extensions,
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

// Router returns the model router.
func (h *Harness) Router() *ModelRouter {
	return h.router
}

// Extensions returns the extension manager.
func (h *Harness) Extensions() *ExtensionManager {
	return h.extensions
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

	// Emit settled event if no more queued messages (Pi parity)
	if !h.agent.HasQueuedMessages() {
		h.hooks.Dispatch(ctx, HookEvent{
			Type: OnSettled,
			Payload: map[string]any{"nextTurnCount": 0},
		})
	}

	// Emit save_point event (Pi parity)
	// Ion doesn't have pendingSessionWrites, so hadPendingMutations is always false
	h.hooks.Dispatch(ctx, HookEvent{
		Type: OnSavePoint,
		Payload: map[string]any{"hadPendingMutations": false},
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

// Compact runs compaction on the session.
// Returns true if compaction occurred.
func (h *Harness) Compact(ctx context.Context) (bool, error) {
	// Dispatch before_compaction hook
	result, err := h.hooks.Dispatch(ctx, HookEvent{
		Type: BeforeCompaction,
		Payload: BeforeCompactionPayload{},
	})
	if err != nil {
		return false, fmt.Errorf("before_compaction hook: %w", err)
	}
	if result.Abort {
		return false, fmt.Errorf("aborted by before_compaction hook: %s", result.Reason)
	}

	// Run compaction
	compacted, err := h.agent.Compact(ctx)

	// Dispatch after_compaction hook
	h.hooks.Dispatch(ctx, HookEvent{
		Type: AfterCompaction,
		Payload: AfterCompactionPayload{},
	})

	return compacted, err
}

// SetModel updates the agent's model.
func (h *Harness) SetModel(model llm.Model) {
	h.agent.SetModel(model)

	// Dispatch model_update hook
	h.hooks.Dispatch(context.Background(), HookEvent{
		Type: OnModelUpdate,
		Payload: map[string]any{"model": model},
	})
}

// SetThinkingLevel updates the agent's thinking level.
func (h *Harness) SetThinkingLevel(level agent.ThinkingLevel) {
	previousLevel := h.agent.State().ThinkingLevel
	h.agent.SetThinkingLevel(level)

	// Dispatch thinking_level_update hook (Pi parity)
	h.hooks.Dispatch(context.Background(), HookEvent{
		Type: OnThinkingLevelUpdate,
		Payload: map[string]any{"level": level, "previousLevel": previousLevel},
	})
}

// SetTools updates the agent's available tools.
func (h *Harness) SetTools(tools []agent.AgentTool) {
	previousTools := h.agent.State().AllTools
	h.agent.SetTools(tools)

	// Dispatch tools_update hook (Pi parity)
	previousToolNames := make([]string, len(previousTools))
	for i, t := range previousTools {
		previousToolNames[i] = t.Name
	}
	toolNames := make([]string, len(tools))
	for i, t := range tools {
		toolNames[i] = t.Name
	}
	h.hooks.Dispatch(context.Background(), HookEvent{
		Type: OnToolsUpdate,
		Payload: map[string]any{"toolNames": toolNames, "previousToolNames": previousToolNames, "source": "set"},
	})
}

// SetActiveTools sets the active tool names.
// Only tools with these names will be available for the next turn.
func (h *Harness) SetActiveTools(toolNames []string) {
	var previousActiveToolNames []string
	for _, t := range h.agent.State().Tools {
		previousActiveToolNames = append(previousActiveToolNames, t.Name)
	}
	h.agent.SetActiveTools(toolNames)

	// Dispatch tools_update hook (Pi parity)
	h.hooks.Dispatch(context.Background(), HookEvent{
		Type: OnToolsUpdate,
		Payload: map[string]any{"activeToolNames": toolNames, "previousActiveToolNames": previousActiveToolNames, "source": "set"},
	})
}

// SetSteeringMode sets the steering queue mode.
func (h *Harness) SetSteeringMode(mode agent.QueueMode) {
	h.agent.SetSteeringMode(mode)
}

// SetFollowUpMode sets the follow-up queue mode.
func (h *Harness) SetFollowUpMode(mode agent.QueueMode) {
	h.agent.SetFollowUpMode(mode)
}

// GetResources returns the current agent resources.
func (h *Harness) GetResources() agent.AgentResources {
	return h.agent.GetResources()
}

// SetResources sets the agent resources (skills and prompt templates).
func (h *Harness) SetResources(resources agent.AgentResources) {
	previousResources := h.agent.GetResources()
	h.agent.SetResources(resources)

	// Dispatch resources_update hook (Pi parity)
	h.hooks.Dispatch(context.Background(), HookEvent{
		Type: OnResourcesUpdate,
		Payload: map[string]any{"resources": resources, "previousResources": previousResources},
	})
}

// GetStreamOptions returns the current stream options.
func (h *Harness) GetStreamOptions() agent.StreamOptions {
	return h.agent.GetStreamOptions()
}

// SetStreamOptions sets the stream options.
func (h *Harness) SetStreamOptions(opts agent.StreamOptions) {
	h.agent.SetStreamOptions(opts)
}

// Skill executes a turn with a skill invocation.
func (h *Harness) Skill(ctx context.Context, name string, additionalInstructions string) ([]agent.AgentMessage, error) {
	return h.agent.Skill(ctx, name, additionalInstructions)
}

// PromptFromTemplate executes a turn with a prompt template.
func (h *Harness) PromptFromTemplate(ctx context.Context, name string, args []string) ([]agent.AgentMessage, error) {
	return h.agent.PromptFromTemplate(ctx, name, args)
}

// AppendMessage appends a message to the conversation history.
func (h *Harness) AppendMessage(msg agent.AgentMessage) {
	h.agent.AppendMessage(msg)
}

// NextTurn queues a message for the next turn.
func (h *Harness) NextTurn(msg agent.AgentMessage) {
	h.agent.NextTurn(msg)
}

// NavigateTree moves the active leaf to the target entry.
// If summarize is true, a branch summary is generated for entries between old leaf and target.
func (h *Harness) NavigateTree(ctx context.Context, targetID string, options agent.NavigateTreeOptions) (agent.NavigateTreeResult, error) {
	// Dispatch before_tree_navigation hook
	result, err := h.hooks.Dispatch(ctx, HookEvent{
		Type: BeforeTreeNavigation,
		Payload: map[string]any{"targetID": targetID, "summarize": options.Summarize},
	})
	if err != nil {
		return agent.NavigateTreeResult{}, fmt.Errorf("before_tree_navigation hook: %w", err)
	}
	if result.Abort {
		return agent.NavigateTreeResult{}, fmt.Errorf("aborted by before_tree_navigation hook: %s", result.Reason)
	}

	// Navigate tree
	treeResult, err := h.agent.NavigateTree(ctx, targetID, options)

	// Dispatch after_tree_navigation hook
	h.hooks.Dispatch(ctx, HookEvent{
		Type: AfterTreeNavigation,
		Payload: map[string]any{"targetID": targetID, "newLeafID": treeResult, "error": err},
	})

	return treeResult, err
}

// Abort aborts the current run.
func (h *Harness) Abort() {
	// Get queued messages before clearing
	var clearedSteer []agent.AgentMessage
	for _, msg := range h.agent.SteeringQueue() {
		clearedSteer = append(clearedSteer, agent.AgentMessage{Role: "user", Parts: []llm.ContentPart{llm.TextPart(msg)}})
	}
	var clearedFollowUp []agent.AgentMessage
	for _, msg := range h.agent.FollowUpQueue() {
		clearedFollowUp = append(clearedFollowUp, agent.AgentMessage{Role: "user", Parts: []llm.ContentPart{llm.TextPart(msg)}})
	}

	h.agent.Abort()

	// Dispatch abort hook (Pi parity)
	h.hooks.Dispatch(context.Background(), HookEvent{
		Type: OnAbort,
		Payload: map[string]any{"clearedSteer": clearedSteer, "clearedFollowUp": clearedFollowUp},
	})
}

// WaitForIdle waits for the agent to reach an idle state.
func (h *Harness) WaitForIdle(ctx context.Context) error {
	return h.agent.WaitForIdle(ctx)
}

// WireContextHook sets up the agent's TransformContext to emit OnContext hooks.
// Call this after creating the harness to enable context hook subscribers.
// If the agent already has a TransformContext, it will be called first,
// then the result is passed to the OnContext hook.
// Hook handlers can return modified messages via HookResult.Data (type ContextResult).
func (h *Harness) WireContextHook() {
	original := h.agent.Config().TransformContext
	h.agent.SetTransformContext(func(ctx context.Context, messages []agent.AgentMessage) []agent.AgentMessage {
		// Call original transform first
		if original != nil {
			messages = original(ctx, messages)
		}
		// Dispatch OnContext hook
		result, _ := h.hooks.Dispatch(ctx, HookEvent{
			Type: OnContext,
			Payload: map[string]any{"messages": messages},
		})
		// Check if handler returned modified messages
		if ctxResult, ok := result.Data.(ContextResult); ok && ctxResult.Messages != nil {
			return ctxResult.Messages
		}
		return messages
	})
}
