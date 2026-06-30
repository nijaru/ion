package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Harness is the stateful session orchestrator. It owns the session, tools,
// model state, queues, hooks, and recovery. The only thing that touches the
// session store. Constructs TurnContext + LoopConfig per turn and calls RunLoop.
//
// Reference: Pi agent-harness.js AgentHarness (line 125).
type Harness struct {
	session session.Session
	store   session.Store
	tools   map[string]Tool
	active  []string // active tool names
	model   llm.Model
	thinking session.ThinkingLevel
	sysprompt string

	// resources (Pi: skills + prompt templates)
	skillsText      string
	promptTemplates map[string]string

	stream func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	auth   func(model llm.Model) (apiKey string, headers map[string]string)
	transport http.RoundTripper
	timeout  time.Duration

	// queues (Pi PendingMessageQueue x3)
	steer    []session.Message
	followUp []session.Message
	nextTurn []session.Message

	// single active run
	phase     Phase
	mu        sync.Mutex
	runDone   chan struct{}
	runCancel chan struct{} // closed to abort current run

	// event subscription
	events         chan session.Event
	externalEvents bool
	listeners      []func(session.Event)

	// hook registry (Pi on/emitHook pattern)
	hooks map[string][]HookHandler

	// buffered session writes during a run
	pending []pendingWrite

	// compaction settings
	compaction CompactionSettings
	contextWindow int
}

type Phase string

const (
	PhaseIdle        Phase = "idle"
	PhaseTurn        Phase = "turn"
	PhaseCompaction  Phase = "compaction"
	PhaseBranchNav   Phase = "branch_summary"
)

// pendingWrite is a buffered session mutation applied at turn boundary.
// Pi reference: agent-harness.js pendSessionWrites (line 410-435).
type pendingWrite struct {
	apply func(ctx context.Context, s session.Session) error
}

// HookHandler is a function registered for a hook type. It receives a payload
// and returns a patch (nil if no change) or an error.
// Reference: Pi agent-harness.js hooks (line 944-960).
type HookHandler func(payload any) (patch any, err error)

// Hook type constants matching Pi's hook names.
const (
	HookBeforeProviderRequest = "before_provider_request"
	HookBeforeProviderPayload = "before_provider_payload"
	HookBeforeAgentStart      = "before_agent_start"
	HookToolResult            = "tool_result"
)

// QueueUpdate is emitted when steer/followUp/nextTurn queues change.
// Reference: Pi agent-harness.js emitQueueUpdate (line 249).
type QueueUpdate struct {
	SteerCount    int
	FollowUpCount int
	NextTurnCount int
}

func (QueueUpdate) IsEvent() {}

// HarnessConfig holds construction-time configuration for a Harness.
type HarnessConfig struct {
	Events chan session.Event
	Session  session.Session
	Store    session.Store
	Tools    []Tool
	Active   []string // active tool names (subset of Tools); nil = all
	Model    llm.Model
	Thinking session.ThinkingLevel
	SysPrompt string
	SkillsText string // pre-formatted skills XML for the system prompt
	PromptTemplates map[string]string // name → template text
	StreamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	Auth     func(model llm.Model) (apiKey string, headers map[string]string)

	// Transport is an optional HTTP transport for provider requests.
	// When set, the BeforeProviderRequest hook can override it.
	Transport http.RoundTripper
	// Timeout is an optional per-request timeout.
	Timeout time.Duration

	// Compaction settings.
	Compaction    CompactionSettings
	ContextWindow int // model context window size in tokens
}

// NewHarness creates a new Harness from the given configuration.
func NewHarness(cfg HarnessConfig) *Harness {
	// Validate tool names — Pi detects duplicates at construction time.
	seen := make(map[string]bool, len(cfg.Tools))
	toolMap := make(map[string]Tool, len(cfg.Tools))
	for _, t := range cfg.Tools {
		if seen[t.Name] {
			panic(fmt.Sprintf("harness: duplicate tool name %q", t.Name))
		}
		seen[t.Name] = true
		toolMap[t.Name] = t
	}
	active := cfg.Active
	if active == nil {
		for _, t := range cfg.Tools {
			active = append(active, t.Name)
		}
	}
	h := &Harness{
		session:  cfg.Session,
		store:    cfg.Store,
		tools:    toolMap,
		active:   active,
		model:    cfg.Model,
		thinking: cfg.Thinking,
		sysprompt: cfg.SysPrompt,
		skillsText: cfg.SkillsText,
		promptTemplates: cfg.PromptTemplates,
		stream:   cfg.StreamFn,
		auth:     cfg.Auth,
		phase: PhaseIdle,
		events:   cfg.Events,
		externalEvents: cfg.Events != nil,
		compaction: cfg.Compaction,
		contextWindow: cfg.ContextWindow,
	}
	if h.events == nil {
		h.events = make(chan session.Event, 64)
	}
	if h.hooks == nil {
		h.hooks = make(map[string][]HookHandler)
	}
	return h
}

// Events returns the channel the TUI subscribes to.
func (h *Harness) Events() <-chan session.Event { return h.events }

// On registers a handler for a hook type. Returns an unsubscribe function.
// Reference: Pi agent-harness.js on (line 962).
func (h *Harness) On(hookType string, handler HookHandler) func() {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.hooks[hookType] = append(h.hooks[hookType], handler)
	ptr := &h.hooks[hookType][len(h.hooks[hookType])-1]
	return func() {
		h.mu.Lock()
		*ptr = nil
		h.mu.Unlock()
	}
}

// emitHook fans out a payload to all handlers registered for hookType.
// Returns collected patches. Uses snapshot-and-release to avoid reentry deadlock.
func (h *Harness) emitHook(hookType string, payload any) (patches []any, err error) {
	h.mu.Lock()
	snapshot := make([]HookHandler, 0, len(h.hooks[hookType]))
	for _, fn := range h.hooks[hookType] {
		if fn != nil {
			snapshot = append(snapshot, fn)
		}
	}
	h.mu.Unlock()
	for _, fn := range snapshot {
		patch, fnErr := fn(payload)
		if fnErr != nil {
			return nil, fnErr
		}
		if patch != nil {
			patches = append(patches, patch)
		}
	}
	return patches, nil
}

// Subscribe registers a listener for all agent events.
// Returns an unsubscribe function. Listeners are called on every emit.
// Reference: Pi agent-harness.js subscribe (line 944).
func (h *Harness) Subscribe(listener func(session.Event)) func() {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.listeners = append(h.listeners, listener)
	// Capture a pointer to the backing array slot. Even if the slice grows and
	// reallocates, we nil the original slot — no leak, just a harmless nil hole.
	ptr := &h.listeners[len(h.listeners)-1]
	return func() {
		h.mu.Lock()
		*ptr = nil
		h.mu.Unlock()
	}
}

// emit sends an event to the TUI channel and all subscribers.
// Snapshot-and-release: grab listeners under the lock, then call them
// without the lock to prevent reentry deadlocks.
func (h *Harness) emit(e session.Event) {
	select {
	case h.events <- e:
	default:
		// channel full; drop event (non-blocking)
	}
	h.mu.Lock()
	snapshot := make([]func(session.Event), 0, len(h.listeners))
	for _, fn := range h.listeners {
		if fn != nil {
			snapshot = append(snapshot, fn)
		}
	}
	h.mu.Unlock()
	for _, fn := range snapshot {
		fn(e)
	}
}

// Prompt submits a user message and runs the agent turn.
// Returns the final assistant message. Blocks until the turn completes.
//
// Reference: Pi agent-harness.js prompt (line 541).
func (h *Harness) Prompt(ctx context.Context, text string) (session.Message, error) {
	h.mu.Lock()
	if h.phase != PhaseIdle {
		h.mu.Unlock()
		return nil, fmt.Errorf("harness is busy (phase=%s)", h.phase)
	}
	h.phase = PhaseTurn
	h.runDone = make(chan struct{})
	h.runCancel = make(chan struct{})
	cancel := h.runCancel
	h.mu.Unlock()

	defer func() {
		h.mu.Lock()
		h.phase = PhaseIdle
		h.runCancel = nil
		close(h.runDone)
		h.runDone = nil
		h.mu.Unlock()
	}()

	// Build turn context from session.
	snap, err := h.session.BuildContext(ctx)
	if err != nil {
		return nil, fmt.Errorf("build context: %w", err)
	}

	// Restore active state from session tree (survives replay).
	// If the session has recorded model/thinking/tool changes, they take precedence
	// over the harness's current state (which may be from a fresh start).
	h.mu.Lock()
	if snap.ActiveModel != "" && snap.ActiveModel != h.model.ID {
		// Model was changed mid-session — the session tree is authoritative.
		// The harness's model at construction time was a starting point.
	}
	if snap.Thinking != "" {
		h.thinking = snap.Thinking
	}
	if len(snap.ActiveTools) > 0 {
		h.active = snap.ActiveTools
	}
	h.mu.Unlock()

	// Drain nextTurn queue and prepend to prompts.
	prompts := h.drainNextTurn()
	prompts = append(prompts, session.NewUserText(text, time.Now()))

	// Emit before_agent_start hook — inject extra messages.
	patches, err := h.emitHook(HookBeforeAgentStart, beforeAgentStartPayload{
		Prompt:       text,
		SystemPrompt: h.sysprompt,
	})
	if err != nil {
		return nil, fmt.Errorf("before_agent_start hook: %w", err)
	}
	for _, p := range patches {
		if bp, ok := p.(*BeforeAgentStartPatch); ok && bp != nil {
			for _, msg := range bp.Messages {
				prompts = append(prompts, msg)
			}
			if bp.SystemPrompt != "" {
				h.sysprompt = bp.SystemPrompt
			}
		}
	}

	// Build tools for the loop.
	tools := h.buildTools()

	// Build LoopConfig.
	cfg := h.buildLoopConfig(ctx, tools)

	// Run the loop with overflow recovery.
	var msgs []session.Message
	for range 2 {
		msgs = RunLoop(ctx, prompts, TurnContext{
			SystemPrompt: h.systemPrompt(),
			Messages:     snap.Messages,
		}, cfg, h.handleEvent, cancel)

		// Check for context overflow error.
		if len(msgs) > 0 {
			if am, ok := msgs[len(msgs)-1].(*session.AssistantMessage); ok {
				if am.StopReason == "error" {
					// Check if it's a context overflow error.
					for _, c := range am.Content {
						if tc, ok := c.(session.TextContent); ok {
							if IsContextOverflowError(fmt.Errorf("%s", tc.Text)) {
								// Compact and retry.
								if compactErr := h.Compact(ctx); compactErr != nil {
									break // can't compact, give up
								}
								// Rebuild context after compaction.
								snap, err = h.session.BuildContext(ctx)
								if err != nil {
									break
								}
								continue // retry
							}
						}
					}
				}
			}
		}
		break // no overflow, done
	}

	// Flush any remaining pending writes.
	h.flushPending(ctx)

	// Return the last assistant message.
	for i := len(msgs) - 1; i >= 0; i-- {
		if am, ok := msgs[i].(*session.AssistantMessage); ok {
			return am, nil
		}
	}
	return nil, fmt.Errorf("no assistant message produced")
}

// handleEvent is the event reducer. Persists on message_end, flushes on turn_end/agent_end.
//
// Reference: Pi agent-harness.js handleAgentEvent (line 441).
func (h *Harness) handleEvent(e session.Event) {
	h.emit(e) // forward to TUI

	ctx := context.Background()

	switch e := e.(type) {
	case session.MessageEnd:
		// Persist the message to the session.
		if _, err := h.session.AppendMessage(ctx, e.Message); err != nil {
			h.emit(&session.Error{Err: fmt.Errorf("persist message: %w", err)})
		}

	case session.TurnEnd:
		h.flushPending(ctx)
		// Persist tool results so PrepareNextTurn's BuildContext sees them.
		for _, tr := range e.ToolResults {
			msg := tr // copy
			if _, err := h.session.AppendMessage(ctx, &msg); err != nil {
				h.emit(&session.Error{Err: fmt.Errorf("persist tool result: %w", err)})
			}
		}
		// Auto-compaction check after turn ends.
		if ShouldCompactAfterTurn(ctx, h.session, h.contextWindow, h.compaction) {
			if err := h.Compact(ctx); err != nil {
				h.emit(&session.Error{Err: fmt.Errorf("auto-compact: %w", err)})
			}
		}

	case session.AgentEnd:
		h.flushPending(ctx)
	}
}

// buildLoopConfig constructs the per-turn LoopConfig.
//
// Reference: Pi agent-harness.js createLoopConfig (line 350).
func (h *Harness) buildLoopConfig(ctx context.Context, tools []Tool) LoopConfig {
	return LoopConfig{
		Model:    h.model,
		Thinking: h.thinking,
		Tools:    tools,
		StreamFn: h.wrapStreamFn(),
		Convert:  DefaultConvert,
		Auth:     h.auth,
		DrainSteer: func() []session.Message {
			return h.drain(&h.steer)
		},
		DrainFollowUp: func() []session.Message {
			return h.drain(&h.followUp)
		},
		AfterToolCall: func(ctx ToolCallResultContext) *ToolCallPatch {
			patches, err := h.emitHook(HookToolResult, toolResultPayload{
				ToolCallID: ctx.ToolCall.ID,
				ToolName:   ctx.ToolCall.Name,
				Input:      ctx.Args,
				Content:    ctx.Result.Content,
				IsError:    ctx.Result.IsError,
			})
			if err != nil || len(patches) == 0 {
				return nil
			}
			// Merge patches. Last non-nil wins per field.
			var merged *ToolCallPatch
			for _, p := range patches {
				if tp, ok := p.(*ToolCallPatch); ok && tp != nil {
					if merged == nil {
						merged = tp
					} else {
						if len(tp.Content) > 0 {
							merged.Content = tp.Content
						}
						if tp.Details != nil {
							merged.Details = tp.Details
						}
						if tp.IsError != nil {
							merged.IsError = tp.IsError
						}
						if tp.Terminate != nil {
							merged.Terminate = tp.Terminate
						}
					}
				}
			}
			return merged
		},
		PrepareNextTurn: func(ctx context.Context) *NextTurnSnapshot {
			h.flushPending(ctx)
			snap, err := h.session.BuildContext(ctx)
			if err != nil {
				return nil
			}
			// Snapshot model and thinking under lock to avoid racing with SetModel/SetThinking.
			h.mu.Lock()
			m := h.model
			t := h.thinking
			h.mu.Unlock()
			return &NextTurnSnapshot{
				Context: TurnContext{
					SystemPrompt: h.systemPrompt(),
					Messages:     snap.Messages,
				},
				Model:    &m,
				Thinking: &t,
			}
		},
	}
}

// toolResultPayload is the payload for HookToolResult.
type toolResultPayload struct {
	ToolCallID string
	ToolName   string
	Input      json.RawMessage
	Content    []session.Content
	IsError    bool
}

// wrapStreamFn wraps the harness stream function to emit provider request/payload hooks.
func (h *Harness) wrapStreamFn() func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	base := h.stream
	return func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		// Snapshot model headers under lock (concurrent-setters-proof).
		h.mu.Lock()
		modelHeaders := map[string]string{}
		for k, v := range h.model.Headers {
			modelHeaders[k] = v
		}
		h.mu.Unlock()

		// Emit before_provider_request hook — allows extensions to patch model headers.
		patches, err := h.emitHook(HookBeforeProviderRequest, beforeProviderRequestPayload{
			Model:     req.Model,
			SessionID: req.SessionID,
			Headers:   modelHeaders,
		})
		if err != nil {
			return nil, fmt.Errorf("before_provider_request hook: %w", err)
		}

		// Apply patches under lock.
		h.mu.Lock()
		for _, p := range patches {
			if bp, ok := p.(*BeforeProviderRequestPatch); ok && bp != nil {
				if bp.Headers != nil {
					if h.model.Headers == nil {
						h.model.Headers = make(map[string]string)
					}
					for k, v := range bp.Headers {
						h.model.Headers[k] = v
					}
				}
				if bp.Transport != nil {
					h.transport = *bp.Transport
				}
				if bp.Timeout != nil {
					h.timeout = *bp.Timeout
				}
			}
		}
		h.mu.Unlock()

		// Emit before_provider_payload hook — transform the raw JSON payload.
		rawPayload, err := json.Marshal(req)
		if err != nil {
			return nil, fmt.Errorf("marshal request for hook: %w", err)
		}
		payloadPatches, err := h.emitHook(HookBeforeProviderPayload, beforeProviderPayloadPayload{
			Model:   req.Model,
			Payload: rawPayload,
		})
		if err != nil {
			return nil, fmt.Errorf("before_provider_payload hook: %w", err)
		}
		for _, p := range payloadPatches {
			if pp, ok := p.(*BeforeProviderPayloadPatch); ok && pp != nil && len(pp.Payload) > 0 {
				if err := json.Unmarshal(pp.Payload, req); err != nil {
					return nil, fmt.Errorf("apply payload patch: %w", err)
				}
			}
		}

		return base(ctx, req)
	}
}

// BeforeProviderRequestPatch is returned by before_provider_request hooks.
type BeforeProviderRequestPatch struct {
	Headers   map[string]string
	Transport *http.RoundTripper
	Timeout   *time.Duration
}

// beforeProviderRequestPayload is the payload for HookBeforeProviderRequest.
type beforeProviderRequestPayload struct {
	Model     string
	SessionID string
	Headers   map[string]string
}

// BeforeProviderPayloadPatch is returned by before_provider_payload hooks.
type BeforeProviderPayloadPatch struct {
	Payload json.RawMessage
}

// beforeProviderPayloadPayload is the payload for HookBeforeProviderPayload.
type beforeProviderPayloadPayload struct {
	Model   string
	Payload json.RawMessage
}

// BeforeAgentStartPatch is returned by before_agent_start hooks.
type BeforeAgentStartPatch struct {
	Messages     []session.Message
	SystemPrompt string // override harness system prompt
}

// beforeAgentStartPayload is the payload for HookBeforeAgentStart.
type beforeAgentStartPayload struct {
	Prompt       string
	Images       []any // TODO: image support
	SystemPrompt string
}

// buildTools builds the Tool slice from the active tool names.
func (h *Harness) buildTools() []Tool {
	tools := make([]Tool, 0, len(h.active))
	for _, name := range h.active {
		if t, ok := h.tools[name]; ok {
			tools = append(tools, t)
		}
	}
	return tools
}

// --- queue management ---

func (h *Harness) drain(queue *[]session.Message) []session.Message {
	msgs := *queue
	*queue = nil
	return msgs
}

func (h *Harness) drainNextTurn() []session.Message {
	return h.drain(&h.nextTurn)
}

// Steer queues a message to be injected before the next assistant response.
func (h *Harness) Steer(text string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.steer = append(h.steer, session.NewUserText(text, time.Now()))
	h.emitQueueUpdate()
}

// FollowUp queues a message to be processed after the agent would otherwise stop.
func (h *Harness) FollowUp(text string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.followUp = append(h.followUp, session.NewUserText(text, time.Now()))
	h.emitQueueUpdate()
}

// NextTurn queues a message to be prepended to the next prompt.
func (h *Harness) NextTurn(text string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.nextTurn = append(h.nextTurn, session.NewUserText(text, time.Now()))
	h.emitQueueUpdate()
}

// emitQueueUpdate notifies subscribers that queued input has changed.
// Must be called with h.mu held.
func (h *Harness) emitQueueUpdate() {
	h.emit(&QueueUpdate{
		SteerCount:    len(h.steer),
		FollowUpCount: len(h.followUp),
		NextTurnCount: len(h.nextTurn),
	})
}

// --- buffered writes ---

func (h *Harness) flushPending(ctx context.Context) {
	for _, pw := range h.pending {
		_ = pw.apply(ctx, h.session)
	}
	h.pending = nil
}

// SetModel changes the model. If a run is active, buffered until next turn boundary.
func (h *Harness) SetModel(model llm.Model) {
	h.mu.Lock()
	defer h.mu.Unlock()
	oldModel := h.model
	h.model = model
	if model.ID != oldModel.ID {
		h.pending = append(h.pending, pendingWrite{
			apply: func(ctx context.Context, s session.Session) error {
				_, err := s.AppendModelChange(ctx, model.Provider, model.ID)
				return err
			},
		})
	}
}

// SetThinking changes the thinking level.
func (h *Harness) SetThinking(level session.ThinkingLevel) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.thinking = level
	h.pending = append(h.pending, pendingWrite{
		apply: func(ctx context.Context, s session.Session) error {
			_, err := s.AppendThinkingLevelChange(ctx, level)
			return err
		},
	})
}

// SetTools changes the active tools.
func (h *Harness) SetTools(tools []Tool, active []string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	toolMap := make(map[string]Tool, len(tools))
	for _, t := range tools {
		toolMap[t.Name] = t
	}
	h.tools = toolMap
	h.active = active
	h.pending = append(h.pending, pendingWrite{
		apply: func(ctx context.Context, s session.Session) error {
			_, err := s.AppendActiveToolsChange(ctx, active)
			return err
		},
	})
}

// WaitForIdle blocks until the current run completes.
func (h *Harness) WaitForIdle() {
	h.mu.Lock()
	done := h.runDone
	h.mu.Unlock()
	if done != nil {
		<-done
	}
}

// Abort cancels the current run.
func (h *Harness) Abort() error {
	h.mu.Lock()
	cancel := h.runCancel
	h.mu.Unlock()
	if cancel != nil {
		select {
		case <-cancel:
			// already closed
		default:
			close(cancel)
		}
	}
	return nil
}

// Close releases resources.
func (h *Harness) Close() error {
	if !h.externalEvents {
		close(h.events)
	}
	return h.session.Close()
}

// Session returns the underlying session handle. Used by TUI for ID(), Usage(), Entries().
func (h *Harness) Session() session.Session { return h.session }

// Store returns the underlying store. Used by TUI for export/tree operations.
func (h *Harness) Store() session.Store { return h.store }

// Compact triggers context compaction.
// Compact performs context compaction on the session.
//
// Reference: Pi agent-harness.js compact (line 500)
func (h *Harness) Compact(ctx context.Context) error {
	_, err := Compact(ctx, h.session, h.stream, h.model.ID, h.compaction)
	if err != nil {
		return fmt.Errorf("compact: %w", err)
	}
	return nil
}

// systemPrompt returns the full system prompt with skills appended.
func (h *Harness) systemPrompt() string {
	if h.skillsText == "" {
		return h.sysprompt
	}
	return h.sysprompt + h.skillsText
}

// PromptFromTemplate fills a prompt template with the given data.
// Returns the filled template, or an empty string if the template doesn't exist.
// Reference: Pi agent.js promptFromTemplate (line 98).
func (h *Harness) PromptFromTemplate(name string, data map[string]string) string {
	tmpl, ok := h.promptTemplates[name]
	if !ok {
		return ""
	}
	result := tmpl
	for k, v := range data {
		result = strings.ReplaceAll(result, "{{"+k+"}}", v)
	}
	return result
}

// GetModel returns the current model.
func (h *Harness) GetModel() llm.Model { h.mu.Lock(); defer h.mu.Unlock(); return h.model }

// GetThinkingLevel returns the current thinking level.
func (h *Harness) GetThinkingLevel() session.ThinkingLevel { h.mu.Lock(); defer h.mu.Unlock(); return h.thinking }

// GetTools returns the current tool map and active tool names.
func (h *Harness) GetTools() (map[string]Tool, []string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	tools := make(map[string]Tool, len(h.tools))
	for k, v := range h.tools {
		tools[k] = v
	}
	active := make([]string, len(h.active))
	copy(active, h.active)
	return tools, active
}

// AppendMessage appends a message directly to the session without running a turn.
// Reference: Pi agent-harness.js appendMessage (line 614).
func (h *Harness) AppendMessage(ctx context.Context, msg session.Message) error {
	_, err := h.session.AppendMessage(ctx, msg)
	return err
}
