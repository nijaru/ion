package harness

import (
	"context"
	"sync"

	"github.com/nijaru/ion/internal/agent"
)

// HookType identifies the type of lifecycle hook.
type HookType string

const (
	// BeforeAgentStart fires before the agent loop starts.
	BeforeAgentStart HookType = "before_agent_start"
	// AfterAgentRun fires after the agent loop completes.
	AfterAgentRun HookType = "after_agent_run"
	// BeforeProviderRequest fires before each provider request.
	BeforeProviderRequest HookType = "before_provider_request"
	// AfterProviderResponse fires after each provider response.
	AfterProviderResponse HookType = "after_provider_response"
	// BeforeToolCall fires before each tool call.
	BeforeToolCall HookType = "before_tool_call"
	// AfterToolCall fires after each tool call.
	AfterToolCall HookType = "after_tool_call"
	// BeforeCompaction fires before compaction.
	BeforeCompaction HookType = "before_compaction"
	// AfterCompaction fires after compaction.
	AfterCompaction HookType = "after_compaction"
	// BeforeTreeNavigation fires before tree navigation.
	BeforeTreeNavigation HookType = "before_tree_navigation"
	// AfterTreeNavigation fires after tree navigation.
	AfterTreeNavigation HookType = "after_tree_navigation"
	// OnError fires on error.
	OnError HookType = "on_error"
	// OnAbort fires on abort.
	OnAbort HookType = "on_abort"
	// OnSettled fires when the harness reaches idle state after all queues are drained.
	OnSettled HookType = "settled"
	// OnSavePoint fires when pending session writes are flushed.
	OnSavePoint HookType = "save_point"
	// OnResourcesUpdate fires when resources (skills, prompt templates) change.
	OnResourcesUpdate HookType = "resources_update"
	// OnQueueUpdate fires when the steer/followUp/nextTurn queues change.
	OnQueueUpdate HookType = "queue_update"
	// OnContext fires before each LLM call with the message context.
	// Can modify messages by returning a ContextResult with new messages.
	OnContext HookType = "context"
	// OnModelUpdate fires when the model changes.
	OnModelUpdate HookType = "model_update"
	// OnThinkingLevelUpdate fires when the thinking level changes.
	OnThinkingLevelUpdate HookType = "thinking_level_update"
	// OnToolsUpdate fires when tools or active tools change.
	OnToolsUpdate HookType = "tools_update"
)

// HookEvent is a lifecycle event dispatched to hooks.
type HookEvent struct {
	Type    HookType
	Payload any
}

// HookResult is the result of a hook execution.
type HookResult struct {
	// Abort indicates whether to abort the operation.
	Abort bool
	// Reason is the reason for aborting.
	Reason string
	// Data is optional hook-specific data.
	Data any
}

// HookHandler is a function that handles a lifecycle event.
type HookHandler func(ctx context.Context, event HookEvent) (HookResult, error)

// BeforeAgentStartPayload is the payload for before_agent_start hook.
type BeforeAgentStartPayload struct {
	Prompts []agent.AgentMessage
}

// AfterAgentRunPayload is the payload for after_agent_run hook.
type AfterAgentRunPayload struct {
	Messages []agent.AgentMessage
	Error    error
}

// ContextResult is the result for the context hook.
// Return this via HookResult.Data to modify messages before LLM call.
type ContextResult struct {
	Messages []agent.AgentMessage
}

// BeforeProviderRequestPayload is the payload for before_provider_request hook.
type BeforeProviderRequestPayload struct {
	Model    string
	Messages []agent.AgentMessage
}

// AfterProviderResponsePayload is the payload for after_provider_response hook.
type AfterProviderResponsePayload struct {
	Model    string
	Response any
	Error    error
}

// BeforeToolCallPayload is the payload for before_tool_call hook.
type BeforeToolCallPayload struct {
	ToolName string
	Args     any
}

// AfterToolCallPayload is the payload for after_tool_call hook.
type AfterToolCallPayload struct {
	ToolName string
	Args     any
	Result   any
	Error    error
}

// BeforeCompactionPayload is the payload for before_compaction hook.
type BeforeCompactionPayload struct {
	FirstKeptEntryId    string
	MessagesToSummarize []agent.AgentMessage
	TurnPrefixMessages  []agent.AgentMessage
	IsSplitTurn         bool
	TokensBefore        int
	PreviousSummary     string
	FileOps             FileOperations
	Settings            CompactionSettings
}

// FileOperations tracks file operations during a turn.
type FileOperations struct {
	Read   []string
	Written []string
	Edited  []string
}

// CompactionSettings contains compaction configuration.
type CompactionSettings struct {
	Enabled         bool
	ReserveTokens   int
	KeepRecentTokens int
}

// AfterCompactionPayload is the payload for after_compaction hook.
type AfterCompactionPayload struct {
	Summary  string
	Messages []agent.AgentMessage
}

// BeforeTreeNavigationPayload is the payload for before_tree_navigation hook.
type BeforeTreeNavigationPayload struct {
	TargetId            string
	OldLeafId           string
	CommonAncestorId    string
	EntriesToSummarize  []agent.AgentMessage
	UserWantsSummary    bool
	CustomInstructions  string
	ReplaceInstructions bool
	Label               string
}

// AfterTreeNavigationPayload is the payload for after_tree_navigation hook.
type AfterTreeNavigationPayload struct {
	NewLeafId     string
	OldLeafId     string
	SummaryEntry  string
	FromHook      bool
}

// HookDispatcher dispatches lifecycle events to registered hooks.
type HookDispatcher struct {
	handlers map[HookType][]HookHandler
	mu       sync.RWMutex
}

// NewHookDispatcher creates a new hook dispatcher.
func NewHookDispatcher() *HookDispatcher {
	return &HookDispatcher{
		handlers: make(map[HookType][]HookHandler),
	}
}

// On registers a hook handler for the given event type.
func (d *HookDispatcher) On(eventType HookType, handler HookHandler) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.handlers[eventType] = append(d.handlers[eventType], handler)
}

// Dispatch dispatches an event to all registered handlers.
// Handlers are called in order. If any handler returns Abort=true, dispatch stops.
func (d *HookDispatcher) Dispatch(ctx context.Context, event HookEvent) (HookResult, error) {
	d.mu.RLock()
	handlers := d.handlers[event.Type]
	d.mu.RUnlock()

	for _, handler := range handlers {
		result, err := handler(ctx, event)
		if err != nil {
			return result, err
		}
		if result.Abort {
			return result, nil
		}
	}

	return HookResult{}, nil
}

// HasHandlers returns true if there are handlers for the given event type.
func (d *HookDispatcher) HasHandlers(eventType HookType) bool {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return len(d.handlers[eventType]) > 0
}

// Clear removes all handlers for the given event type.
func (d *HookDispatcher) Clear(eventType HookType) {
	d.mu.Lock()
	defer d.mu.Unlock()
	delete(d.handlers, eventType)
}

// ClearAll removes all handlers.
func (d *HookDispatcher) ClearAll() {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.handlers = make(map[HookType][]HookHandler)
}
