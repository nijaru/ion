package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"sync"
	"time"

	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Harness is the stateful session orchestrator. It owns the session, tools,
// model state, queues, hooks, and recovery. The only thing that touches the
// session store. Constructs TurnContext + LoopConfig per turn and calls RunLoop.
//
// Reference: Pi agent-harness.js AgentHarness (line 125).
type Harness struct {
	session   session.Session
	store     session.Store
	tools     map[string]Tool
	active    []string // active tool names
	model     llm.Model
	thinking  session.ThinkingLevel
	sysprompt string
	log       *slog.Logger // structured logger, may be nil
	metrics   *Metrics     // runtime statistics, may be nil

	// resources (Pi: prompt templates; system prompt is preassembled at startup)
	promptTemplates map[string]string

	stream    func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	auth      func(model llm.Model) (apiKey string, headers map[string]string)
	transport http.RoundTripper
	timeout   time.Duration

	// queues (Pi PendingMessageQueue x3)
	steer    []session.Message
	followUp []session.Message
	nextTurn []session.Message

	// queue drain modes (Pi: one-at-a-time vs all)
	steeringMode string // "one-at-a-time" | "all"
	followUpMode string // "one-at-a-time" | "all"

	// single active run
	phase     Phase
	closed    bool
	mu        sync.Mutex
	runDone   chan struct{}
	runCancel chan struct{} // closed to abort current run

	// event subscription
	events         chan session.Event
	externalEvents bool
	delivery       *eventDelivery
	done           chan struct{}
	listeners      []func(session.Event)

	// hook registry (Pi on/emitHook pattern)
	hooks map[string][]HookHandler

	// buffered session writes during a run
	pending []pendingWrite

	// compaction settings
	compaction    CompactionSettings
	contextWindow int
}

type Phase string

const (
	PhaseIdle       Phase = "idle"
	PhaseTurn       Phase = "turn"
	PhaseCompaction Phase = "compaction"
	PhaseBranchNav  Phase = "branch_summary"
)

// pendingWrite is a buffered session mutation applied at turn boundary.
// Pi reference: agent-harness.js pendSessionWrites (line 410-435).
type pendingWrite struct {
	apply      func(ctx context.Context, s session.Session) error
	applyStore func(ctx context.Context, store session.Store) error
}

// HookHandler is a function registered for a hook type. It receives a payload
// and returns a patch (nil if no change) or an error.
// Reference: Pi agent-harness.js hooks (line 944-960).
type HookHandler func(payload any) (patch any, err error)

// Hook type constants matching Pi's hook names.
const (
	HookBeforeProviderRequest = "before_provider_request"
	HookBeforeProviderPayload = "before_provider_payload"
	HookAfterProviderResponse = "after_provider_response"
	HookBeforeAgentStart      = "before_agent_start"
	HookBeforeToolCall        = "before_tool_call"
	HookToolResult            = "tool_result"
)

// QueueUpdate was moved to session/events.go as part of Phase B harness parity.
// The session.QueueUpdate carries full []Message arrays, not just counts.

// HarnessConfig holds construction-time configuration for a Harness.
type HarnessConfig struct {
	Events          chan session.Event
	Session         session.Session
	Store           session.Store
	Tools           []Tool
	Active          []string // active tool names (subset of Tools); nil = all
	Model           llm.Model
	Thinking        session.ThinkingLevel
	SysPrompt       string
	PromptTemplates map[string]string // name → template text
	StreamFn        func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	Auth            func(model llm.Model) (apiKey string, headers map[string]string)

	// Transport is an optional HTTP transport for provider requests.
	// When set, the BeforeProviderRequest hook can override it.
	Transport http.RoundTripper
	// Timeout is an optional per-request timeout.
	Timeout time.Duration

	// Logger is used for structured logging throughout the harness lifecycle.
	// When nil, logging is silent.
	Logger *slog.Logger

	// SteeringMode controls how steering messages are drained (default "one-at-a-time").
	SteeringMode string
	// FollowUpMode controls how follow-up messages are drained (default "one-at-a-time").
	FollowUpMode string

	// Metrics collects runtime statistics. When nil, metrics are not recorded.
	Metrics *Metrics

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
		session:         cfg.Session,
		store:           cfg.Store,
		tools:           toolMap,
		active:          active,
		model:           cfg.Model,
		thinking:        cfg.Thinking,
		sysprompt:       cfg.SysPrompt,
		log:             cfg.Logger,
		metrics:         cfg.Metrics,
		promptTemplates: cfg.PromptTemplates,
		stream:          cfg.StreamFn,
		auth:            cfg.Auth,
		phase:           PhaseIdle,
		events:          cfg.Events,
		externalEvents:  cfg.Events != nil,
		done:            make(chan struct{}),
		compaction:      cfg.Compaction,
		contextWindow:   cfg.ContextWindow,
		steeringMode:    cfg.SteeringMode,
		followUpMode:    cfg.FollowUpMode,
	}
	if h.steeringMode == "" {
		h.steeringMode = "one-at-a-time"
	}
	if h.followUpMode == "" {
		h.followUpMode = "one-at-a-time"
	}
	if h.events == nil {
		h.events = make(chan session.Event, 256)
	}
	h.delivery = newEventDelivery(h.events, !h.externalEvents)
	if h.hooks == nil {
		h.hooks = make(map[string][]HookHandler)
	}
	return h
}

// Events returns the ordered channel the TUI subscribes to.
func (h *Harness) Events() <-chan session.Event { return h.events }

// Done is closed when the harness is no longer a valid event source.
func (h *Harness) Done() <-chan struct{} { return h.done }

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
	index := len(h.listeners)
	h.listeners = append(h.listeners, listener)
	return func() {
		h.mu.Lock()
		if index < len(h.listeners) {
			h.listeners[index] = nil
		}
		h.mu.Unlock()
	}
}

// emit snapshots listeners without holding h.mu, then enqueues the event on
// the single ordered delivery path. The dispatcher invokes listeners before it
// sends the same event to the consumer channel.
func (h *Harness) emit(e session.Event) {
	h.mu.Lock()
	snapshot := make([]func(session.Event), 0, len(h.listeners))
	for _, fn := range h.listeners {
		if fn != nil {
			snapshot = append(snapshot, fn)
		}
	}
	delivery := h.delivery
	h.mu.Unlock()

	if delivery != nil {
		delivery.enqueue(e, snapshot)
	}
}

// Prompt submits a user message and runs the agent turn.
// Returns the final assistant message. Blocks until the turn completes.
//
// Reference: Pi agent-harness.js prompt (line 541).
func (h *Harness) Prompt(ctx context.Context, text string) (session.Message, error) {
	turnStart := time.Now()
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return nil, errors.New("harness is closed")
	}
	if h.phase != PhaseIdle {
		h.mu.Unlock()
		h.logf(slog.LevelWarn, "prompt rejected: harness busy", slog.String("phase", string(h.phase)))
		return nil, fmt.Errorf("harness is busy (phase=%s)", h.phase)
	}
	h.phase = PhaseTurn
	h.runDone = make(chan struct{})
	h.runCancel = make(chan struct{})
	cancel := h.runCancel
	h.mu.Unlock()

	defer func() {
		dur := time.Since(turnStart)
		h.mu.Lock()
		h.phase = PhaseIdle
		h.runCancel = nil
		done := h.runDone
		if done != nil {
			close(done)
			h.runDone = nil
		}
		modelID := h.model.ID
		h.mu.Unlock()
		if h.metrics != nil {
			h.metrics.RecordTurn(dur)
		}
		h.logf(slog.LevelInfo, "turn end", slog.Duration("duration", dur), slog.String("model", modelID))
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
	prompts := h.drainNextTurn() // holds its own lock
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
			prompts = append(prompts, bp.Messages...)
			if bp.SystemPrompt != "" {
				h.sysprompt = bp.SystemPrompt
			}
		}
	}

	// Build tools for the loop.
	tools := h.buildTools()

	// Run the loop with overflow recovery. The harness owns the single terminal
	// AgentEnd (DESIGN §1.3): RunLoop still emits AgentEnd internally, but we
	// capture and suppress it here and emit exactly ONE after the (possibly
	// retried) run completes, so the TUI never sees a double AgentEnd.
	var msgs []session.Message
	var lastAgentEnd session.AgentEnd
	var persistErr error
	recordPersistErr := func(err error) {
		if err == nil || persistErr != nil {
			return
		}
		persistErr = err
		h.emit(&session.Error{Err: err})
		select {
		case <-cancel:
		default:
			close(cancel)
		}
	}
	// Build LoopConfig after the persistence failure callback exists so
	// PrepareNextTurn can stop the run when a buffered write fails.
	cfg := h.buildLoopConfig(ctx, tools, recordPersistErr)
	emitWrap := func(e session.Event) {
		if ae, ok := e.(session.AgentEnd); ok {
			lastAgentEnd = ae
			return // harness emits the single terminal AgentEnd below
		}
		if persistErr != nil {
			return
		}
		if err := h.handleEvent(ctx, e); err != nil {
			recordPersistErr(err)
		}
	}
	for attempt := 0; attempt < 2; attempt++ {
		func() {
			// Recover from panics in RunLoop: emit failure message + lifecycle events.
			// Reference: Pi agent-harness.js emitRunFailure (line 471).
			defer func() {
				if r := recover(); r != nil {
					err := fmt.Errorf("agent loop panic: %v", r)
					msg := newFailureMessage(h.model, err, false, h.thinking)
					failureMsg := &msg
					emitWrap(session.MessageStart{Message: failureMsg})
					emitWrap(session.MessageEnd{Message: failureMsg})
					emitWrap(session.TurnEnd{Message: *failureMsg})
					emitWrap(session.AgentEnd{Messages: []session.Message{failureMsg}})
					msgs = []session.Message{failureMsg}
					h.logf(slog.LevelError, "loop panic recovered", slog.String("error", err.Error()))
				}
			}()
			msgs = RunLoop(ctx, prompts, TurnContext{
				SystemPrompt: h.sysprompt,
				Messages:     snap.Messages,
			}, cfg, emitWrap, cancel)
		}()
		if persistErr != nil {
			break
		}

		// After the first attempt, prompts have been persisted to the session tree.
		// If the turn overflows and we compact + retry, the rebuilt context already
		// contains the user message. Re-sending prompts would duplicate them.
		prompts = nil

		// Overflow is surfaced by providers as a request error in
		// AssistantMessage.Error with empty Content, but some paths surface it
		// inline in the content text. Check both.
		overflow := false
		if len(msgs) > 0 {
			if am, ok := msgs[len(msgs)-1].(*session.AssistantMessage); ok {
				check := am.Error
				if check == "" {
					for _, c := range am.Content {
						if tc, ok := c.(session.TextContent); ok {
							check = tc.Text
							break
						}
					}
				}
				if check != "" && IsContextOverflowError(fmt.Errorf("%s", check)) {
					overflow = true
				}
			}
		}
		if !overflow {
			break // no overflow, done
		}
		// Compact and retry.
		if compactErr := h.Compact(ctx); compactErr != nil {
			break // can't compact, give up
		}
		snap, err = h.session.BuildContext(ctx)
		if err != nil {
			break
		}
	}

	if persistErr != nil {
		return nil, persistErr
	}

	// Emit exactly one terminal AgentEnd for the whole run (DESIGN §1.3).
	if lastAgentEnd.Messages == nil {
		lastAgentEnd = session.AgentEnd{Messages: msgs}
	}
	if err := h.handleEvent(ctx, lastAgentEnd); err != nil {
		recordPersistErr(err)
		return nil, persistErr
	}

	// handleEvent(AgentEnd) flushes pending writes before terminal lifecycle
	// events. Do not flush again after AgentEnd: a concurrent setter may queue
	// a mutation after the terminal boundary, which belongs to the next turn.

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
// Pi invariant: message_end persists BEFORE emitting to subscribers so that
// subsequent BuildContext calls (e.g., from PrepareNextTurn) see the message.
// If persistence fails, handleEvent returns before MessageEnd; Prompt emits
// an Error and cancels the run instead of acknowledging a non-durable turn.
func (h *Harness) handleEvent(ctx context.Context, e session.Event) error {
	if ctx == nil {
		ctx = context.Background()
	}

	switch e := e.(type) {
	case session.TurnStart:
		h.emit(e) // forward to TUI

	case session.MessageStart:
		h.emit(e)

	case session.MessageEnd:
		// Persist message to session tree BEFORE emitting to subscribers.
		// Pi: orders appendMessage before emitAny so subscribers can BuildContext.
		if _, err := h.session.AppendMessage(ctx, e.Message); err != nil {
			err = fmt.Errorf("persist message: %w", err)
			h.logf(slog.LevelError, "persist message failed", slog.String("error", err.Error()))
			return err
		}
		h.logMessage(e.Message)
		h.emit(e)

	case session.TurnEnd:
		hadPending := len(h.pending) > 0
		if err := h.flushPending(ctx); err != nil {
			return err
		}
		// Emit SavePoint after durable writes (Pi: line ~480). HadPendingMutations
		// must be captured BEFORE flush, otherwise always false.
		h.emit(session.SavePoint{HadPendingMutations: hadPending})
		// Auto-compaction check after turn ends.
		if ShouldCompactAfterTurn(ctx, h.session, h.contextWindow, h.compaction) {
			if err := h.Compact(ctx); err != nil {
				h.emit(&session.Error{Err: fmt.Errorf("auto-compact: %w", err)})
			}
		}
		// Forward TurnEnd to the TUI so it can call handleTurnFinished.
		h.emit(e)

	case session.AgentEnd:
		if err := h.flushPending(ctx); err != nil {
			return err
		}
		h.mu.Lock()
		nextCount := len(h.nextTurn)
		h.mu.Unlock()
		// DESIGN says Settled after agent_end; Pi emits AgentEnd then settled.
		// Emit AgentEnd first (terminal), then Settled (idle signal).
		h.emit(e) // AgentEnd is terminal per DESIGN §1.3
		h.emit(session.Settled{NextTurnCount: nextCount})

	default:
		// Forward all other events (ToolExecStart, ToolExecEnd, etc.) to TUI.
		h.emit(e)
	}
	return nil
}

// buildLoopConfig constructs the per-turn LoopConfig.
//
// Reference: Pi agent-harness.js createLoopConfig (line 350).
func (h *Harness) buildLoopConfig(ctx context.Context, tools []Tool, onPersistenceError func(error)) LoopConfig {
	// Snapshot mutable harness fields under lock to avoid racing with
	// SetModel / SetThinking / SetTools from the TUI goroutine.
	h.mu.Lock()
	model := h.model
	thinking := h.thinking
	h.mu.Unlock()

	return LoopConfig{
		Model:     model,
		Thinking:  thinking,
		SessionID: h.session.Meta().ID,
		Tools:     tools,
		StreamFn:  h.wrapStreamFn(),
		Convert:   DefaultConvert,
		Auth:      h.auth,
		DrainSteer: func() []session.Message {
			h.mu.Lock()
			msgs := h.drainQueued(&h.steer, h.steeringMode)
			h.mu.Unlock()
			// Pi: emitQueueUpdate after draining (agent-harness.js:337).
			// Must emit outside lock — emit() acquires h.mu for listener snapshot.
			h.emitQueueUpdate()
			return msgs
		},
		DrainFollowUp: func() []session.Message {
			h.mu.Lock()
			msgs := h.drainQueued(&h.followUp, h.followUpMode)
			h.mu.Unlock()
			h.emitQueueUpdate()
			return msgs
		},
		BeforeToolCall: func(ctx ToolCallContext) *ToolCallDecision {
			patches, err := h.emitHook(HookBeforeToolCall, beforeToolCallPayload{
				ToolCallID: ctx.ToolCall.ID,
				ToolName:   ctx.ToolCall.Name,
				Args:       ctx.Args,
			})
			if err != nil || len(patches) == 0 {
				return nil
			}
			for _, p := range patches {
				if bp, ok := p.(*ToolCallDecision); ok && bp != nil {
					return bp
				}
			}
			return nil
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
		PrepareNextTurn: func(ctx context.Context, toolResults []session.ToolResultMessage) *NextTurnSnapshot {
			// Tool results are already persisted (handleEvent persists before emit).
			// Flush any pending writes (model/thinking changes, custom entries).
			if err := h.flushPending(ctx); err != nil {
				onPersistenceError(err)
				return nil
			}
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
					SystemPrompt: h.sysprompt,
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

// beforeToolCallPayload is the payload for HookBeforeToolCall.
type beforeToolCallPayload struct {
	ToolCallID string
	ToolName   string
	Args       json.RawMessage
}

// wrapStreamFn wraps the harness stream function to emit provider request/payload hooks.
func (h *Harness) wrapStreamFn() func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	base := h.stream
	return func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		// Snapshot model headers and ID under lock (concurrent-setters-proof).
		h.mu.Lock()
		modelID := h.model.ID
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

		// Call the base stream function.
		stream, err := base(ctx, req)

		// Emit after_provider_response: subscriber event + hook for registered handlers.
		// Pi reference: agent-harness.js createStreamFn streamSimple onResponse (line ~327).
		h.emit(session.AfterProviderResponse{})
		h.emitHook(HookAfterProviderResponse, afterProviderResponsePayload{
			Model: modelID,
		})

		return stream, err
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

// afterProviderResponsePayload is the payload for HookAfterProviderResponse.
// Reference: Pi agent-harness.js after_provider_response event (line ~327).
type afterProviderResponsePayload struct {
	Model string
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
// Snapshots h.active and h.tools under lock to avoid racing with SetTools.
func (h *Harness) buildTools() []Tool {
	h.mu.Lock()
	active := make([]string, len(h.active))
	copy(active, h.active)
	// Snapshot tool map to avoid concurrent map read/write with SetTools.
	toolMap := make(map[string]Tool, len(h.tools))
	for k, v := range h.tools {
		toolMap[k] = v
	}
	h.mu.Unlock()

	tools := make([]Tool, 0, len(active))
	for _, name := range active {
		if t, ok := toolMap[name]; ok {
			tools = append(tools, t)
		}
	}
	return tools
}

// --- queue management ---

// drainQueued drains messages from a queue according to the drain mode.
// "one-at-a-time" returns a single message; "all" returns the full queue.
// Must be called under h.mu. Callers must emit QueueUpdate after releasing h.mu.
// Reference: Pi agent-harness.js drainQueuedMessages (line 337).
func (h *Harness) drainQueued(queue *[]session.Message, mode string) []session.Message {
	var msgs []session.Message
	if mode == "all" {
		msgs = *queue
		*queue = nil
	} else {
		if len(*queue) == 0 {
			return nil
		}
		msgs = []session.Message{(*queue)[0]}
		*queue = (*queue)[1:]
	}
	return msgs
}

// drainNextTurn drains the nextTurn queue — always in "all" mode.
// Holds h.mu to avoid racing with NextTurn(). Emits QueueUpdate after draining.
func (h *Harness) drainNextTurn() []session.Message {
	h.mu.Lock()
	msgs := h.drainQueued(&h.nextTurn, "all")
	h.mu.Unlock()
	h.emitQueueUpdate()
	return msgs
}

// Steer queues a message to be injected before the next assistant response.
// Returns an error if the harness is idle (Pi: steer/followUp reject while idle).
func (h *Harness) Steer(text string) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	if h.phase == PhaseIdle {
		h.mu.Unlock()
		return fmt.Errorf("cannot steer while idle")
	}
	h.steer = append(h.steer, session.NewUserText(text, time.Now()))
	steer := make([]session.Message, len(h.steer))
	copy(steer, h.steer)
	followUp := make([]session.Message, len(h.followUp))
	copy(followUp, h.followUp)
	nextTurn := make([]session.Message, len(h.nextTurn))
	copy(nextTurn, h.nextTurn)
	h.mu.Unlock()
	// emit outside lock — emit() acquires h.mu internally for listener snapshot
	h.emit(&session.QueueUpdate{Steer: steer, FollowUp: followUp, NextTurn: nextTurn})
	return nil
}

// FollowUp queues a message to be processed after the agent would otherwise stop.
// Returns an error if the harness is idle.
func (h *Harness) FollowUp(text string) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	if h.phase == PhaseIdle {
		h.mu.Unlock()
		return fmt.Errorf("cannot follow up while idle")
	}
	h.followUp = append(h.followUp, session.NewUserText(text, time.Now()))
	steer := make([]session.Message, len(h.steer))
	copy(steer, h.steer)
	followUp := make([]session.Message, len(h.followUp))
	copy(followUp, h.followUp)
	nextTurn := make([]session.Message, len(h.nextTurn))
	copy(nextTurn, h.nextTurn)
	h.mu.Unlock()
	// emit outside lock — emit() acquires h.mu internally for listener snapshot
	h.emit(&session.QueueUpdate{Steer: steer, FollowUp: followUp, NextTurn: nextTurn})
	return nil
}

// NextTurn queues a message to be prepended to the next prompt (always allowed).
func (h *Harness) NextTurn(text string) {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return
	}
	h.nextTurn = append(h.nextTurn, session.NewUserText(text, time.Now()))
	steer := make([]session.Message, len(h.steer))
	copy(steer, h.steer)
	followUp := make([]session.Message, len(h.followUp))
	copy(followUp, h.followUp)
	nextTurn := make([]session.Message, len(h.nextTurn))
	copy(nextTurn, h.nextTurn)
	h.mu.Unlock()
	// emit outside lock — emit() acquires h.mu internally for listener snapshot
	h.emit(&session.QueueUpdate{Steer: steer, FollowUp: followUp, NextTurn: nextTurn})
}

// emitQueueUpdate emits a QueueUpdate event for tests and internal callers
// that already hold h.mu. Must NOT be called under h.mu — emit() locks.
func (h *Harness) emitQueueUpdate() {
	steer := make([]session.Message, len(h.steer))
	copy(steer, h.steer)
	followUp := make([]session.Message, len(h.followUp))
	copy(followUp, h.followUp)
	nextTurn := make([]session.Message, len(h.nextTurn))
	copy(nextTurn, h.nextTurn)
	h.emit(&session.QueueUpdate{
		Steer: steer, FollowUp: followUp, NextTurn: nextTurn,
	})
}

// --- buffered writes ---

func (h *Harness) flushPending(ctx context.Context) error {
	h.mu.Lock()
	pending := h.pending
	h.pending = nil
	h.mu.Unlock()
	for i, pw := range pending {
		var err error
		if pw.applyStore != nil {
			err = pw.applyStore(ctx, h.store)
		} else {
			err = pw.apply(ctx, h.session)
		}
		if err == nil {
			continue
		}
		err = fmt.Errorf("flush pending write: %w", err)
		h.mu.Lock()
		// Keep the failed write and everything after it ahead of writes queued
		// concurrently, preserving retry order without losing mutations.
		h.pending = append(pending[i:], h.pending...)
		h.mu.Unlock()
		h.logf(slog.LevelError, "flush pending write failed", slog.String("error", err.Error()))
		return err
	}
	return nil
}

// SetModel changes the model. If a run is active, buffered until next turn boundary.
func (h *Harness) SetModel(model llm.Model) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
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
	if h.closed {
		return
	}
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
	if h.closed {
		return
	}
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

// cancelActiveRun clears pending queues and signals the current run without
// waiting for its provider or tools to return. The caller chooses the wait policy.
func (h *Harness) cancelActiveRun() ([]session.Message, []session.Message, error) {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return nil, nil, errors.New("harness is closed")
	}
	clearedSteer := append([]session.Message(nil), h.steer...)
	clearedFollowUp := append([]session.Message(nil), h.followUp...)
	h.steer = nil
	h.followUp = nil
	cancel := h.runCancel
	h.mu.Unlock()

	if cancel != nil {
		select {
		case <-cancel:
		default:
			close(cancel)
		}
	}
	return clearedSteer, clearedFollowUp, nil
}

// Abort cancels the current run and clears steering/follow-up queues.
// Emits an Abort event with the cleared messages (Pi: line ~905).
func (h *Harness) Abort() ([]session.Message, []session.Message, error) {
	clearedSteer, clearedFollowUp, err := h.cancelActiveRun()
	if err != nil {
		return nil, nil, err
	}
	h.emitQueueUpdate()
	h.WaitForIdle()
	h.emit(session.Abort{
		ClearedSteer:    clearedSteer,
		ClearedFollowUp: clearedFollowUp,
	})
	return clearedSteer, clearedFollowUp, nil
}

// Close releases resources. Active work is cancelled before waiting for its
// completion so providers and tools that honor the run signal can terminate.
func (h *Harness) Close() error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return nil
	}
	h.closed = true
	close(h.done)
	cancel := h.runCancel
	h.mu.Unlock()

	if cancel != nil {
		select {
		case <-cancel:
		default:
			close(cancel)
		}
	}
	// Ensure no active run is enqueueing before cancelling the dispatcher.
	h.WaitForIdle()
	flushErr := h.flushPending(context.Background())
	if h.delivery != nil {
		h.delivery.close()
	}
	return errors.Join(flushErr, h.session.Close())
}

// Shutdown attempts a graceful stop: abort any running turn, wait for completion
// (up to the context deadline), flush pending writes, and close resources.
func (h *Harness) Shutdown(ctx context.Context) error {
	if ctx == nil {
		ctx = context.Background()
	}
	h.logf(slog.LevelInfo, "shutdown start")
	if _, _, err := h.cancelActiveRun(); err != nil {
		return err
	}
	h.emitQueueUpdate()

	h.mu.Lock()
	done := h.runDone
	h.mu.Unlock()
	if done != nil {
		select {
		case <-done:
		case <-ctx.Done():
			h.logf(slog.LevelWarn, "shutdown timed out waiting for turn")
			return ctx.Err()
		}
	}

	if err := h.flushPending(context.Background()); err != nil {
		h.logf(slog.LevelError, "shutdown pending write failed", slog.String("error", err.Error()))
		return errors.Join(err, h.Close())
	}
	h.logf(slog.LevelInfo, "shutdown complete")
	return h.Close()
}

// Session returns the underlying session handle. Used by TUI for ID(), Usage(), Entries().
func (h *Harness) Session() session.Session { return h.session }

// Store returns the underlying store. Used by TUI for tree reads.
func (h *Harness) Store() session.Store { return h.store }

// ExportSessionBundle performs explicit transport through the harness owner.
func (h *Harness) ExportSessionBundle(ctx context.Context, sessionID string) (ionexport.SessionBundle, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return ionexport.SessionBundle{}, errors.New("harness is closed")
	}
	exporter, ok := h.store.(ionexport.SessionBundleExporter)
	if !ok {
		return ionexport.SessionBundle{}, errors.New("session store does not support export")
	}
	return exporter.ExportSessionBundle(ctx, sessionID)
}

// ImportSessionBundle performs explicit transport through the harness owner.
func (h *Harness) ImportSessionBundle(ctx context.Context, bundle ionexport.SessionBundle) (string, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return "", errors.New("harness is closed")
	}
	if h.phase != PhaseIdle {
		return "", fmt.Errorf("harness is busy (phase=%s)", h.phase)
	}
	importer, ok := h.store.(ionexport.SessionBundleImporter)
	if !ok {
		return "", errors.New("session store does not support import")
	}
	return importer.ImportSessionBundle(ctx, bundle)
}

// Metrics returns the runtime metrics collector (may be nil).
func (h *Harness) Metrics() *Metrics { return h.metrics }

// Compact triggers context compaction.
func (h *Harness) Compact(ctx context.Context) error {
	start := time.Now()
	// Build auth from harness config.
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	model := h.model
	thinking := h.thinking
	h.logf(slog.LevelInfo, "compact start", slog.String("model", model.ID))
	h.mu.Unlock()

	var apiKey string
	var authHeaders map[string]string
	if h.auth != nil {
		apiKey, authHeaders = h.auth(model)
	}

	_, err := Compact(ctx, h.session, CompactOptions{
		Model:          model.ID,
		ModelMaxTokens: model.MaxTokens,
		APIKey:         apiKey,
		Headers:        authHeaders,
		ThinkingLevel:  thinking,
		Convert:        DefaultConvert,
		StreamFn:       h.stream,
	}, h.compaction)
	if err != nil {
		h.logf(slog.LevelError, "compact failed", slog.Duration("duration", time.Since(start)), slog.String("error", err.Error()))
		return fmt.Errorf("compact: %w", err)
	}
	h.logf(slog.LevelInfo, "compact end", slog.Duration("duration", time.Since(start)))
	return nil
}

// logMessage logs a message at the appropriate level based on its type.
func (h *Harness) logMessage(msg session.Message) {
	switch msg := msg.(type) {
	case *session.UserMessage:
		text := messageText(msg)
		if len(text) > 80 {
			text = text[:80] + "..."
		}
		h.logf(slog.LevelInfo, "prompt", slog.String("text", text))
	case *session.AssistantMessage:
		text := messageText(msg)
		if len(text) > 120 {
			text = text[:120] + "..."
		}
		h.logf(slog.LevelInfo, "response",
			slog.String("stop_reason", string(msg.StopReason)),
			slog.String("text", text),
		)
	case *session.ToolResultMessage:
		text := messageText(msg)
		if len(text) > 120 {
			text = text[:120] + "..."
		}
		h.logf(slog.LevelInfo, "tool_result",
			slog.String("tool", msg.ToolName),
			slog.Bool("is_error", msg.IsError),
			slog.String("text", text),
		)
	}
}

// messageText returns the combined text content of any message type.
func messageText(msg session.Message) string {
	var text string
	switch m := msg.(type) {
	case *session.UserMessage:
		for _, c := range m.Content {
			if tc, ok := c.(session.TextContent); ok {
				text += tc.Text
			}
		}
	case *session.AssistantMessage:
		for _, c := range m.Content {
			switch c := c.(type) {
			case session.TextContent:
				text += c.Text
			case session.ThinkingContent:
				// skip thinking — it's verbose
			case *session.ToolCall:
				text += fmt.Sprintf("<tool:%s>", c.Name)
			}
		}
	case *session.ToolResultMessage:
		for _, c := range m.Content {
			if tc, ok := c.(session.TextContent); ok {
				text += tc.Text
			}
		}
	}
	return text
}

// logf logs a structured message if the logger is set.
func (h *Harness) logf(level slog.Level, msg string, attrs ...slog.Attr) {
	if h.log == nil {
		return
	}
	a := slog.Record{Time: time.Now(), Level: level, Message: msg}
	a.AddAttrs(attrs...)
	_ = h.log.Handler().Handle(context.Background(), a)
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
func (h *Harness) GetThinkingLevel() session.ThinkingLevel {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.thinking
}

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
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return errors.New("harness is closed")
	}
	_, err := h.session.AppendMessage(ctx, msg)
	return err
}

func appendRunnerEntry(ctx context.Context, store session.Store, entry session.Entry) error {
	if custom, ok := entry.(*session.CustomEntry); ok && custom.ParentID() == "" {
		copy := *custom
		copy.EntryBase.ParentID = store.GetLeafID()
		entry = &copy
	}
	_, err := store.AppendLeafEntry(ctx, entry)
	return err
}

// PersistEntry persists an auxiliary entry through the harness-owned session.
func (h *Harness) PersistEntry(ctx context.Context, entry session.Entry) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return errors.New("harness is closed")
	}
	if h.store == nil {
		return errors.New("harness has no session store")
	}
	if h.phase != PhaseIdle {
		h.pending = append(h.pending, pendingWrite{applyStore: func(ctx context.Context, store session.Store) error {
			return appendRunnerEntry(ctx, store, entry)
		}})
		return nil
	}
	return appendRunnerEntry(ctx, h.store, entry)
}

// AppendSessionInfo persists the session display name.
func (h *Harness) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return "", errors.New("harness is closed")
	}
	if h.phase != PhaseIdle {
		return "", fmt.Errorf("harness is busy (phase=%s)", h.phase)
	}
	return h.session.AppendSessionInfo(ctx, name)
}

// MoveTo switches the session leaf pointer to the given entry ID.
// Optionally appends a branch summary entry. Returns the summary entry ID
// if a summary was provided, "" otherwise.
//
// Reference: Pi session.js moveTo (line 191).
func (h *Harness) MoveTo(ctx context.Context, entryID string, summary *session.BranchSummaryData) (string, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return "", errors.New("harness is closed")
	}
	if h.phase != PhaseIdle {
		return "", fmt.Errorf("harness is busy (phase=%s)", h.phase)
	}
	return h.session.MoveTo(ctx, entryID, summary)
}

// AppendLabel attaches a label to a target entry.
func (h *Harness) AppendLabel(ctx context.Context, targetID, label string) (string, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return "", errors.New("harness is closed")
	}
	if h.phase != PhaseIdle {
		return "", fmt.Errorf("harness is busy (phase=%s)", h.phase)
	}
	return h.session.AppendLabel(ctx, targetID, label)
}

// GetLabel returns the most recent label for a target entry.
func (h *Harness) GetLabel(ctx context.Context, targetID string) (string, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return "", errors.New("harness is closed")
	}
	if h.phase != PhaseIdle {
		return "", fmt.Errorf("harness is busy (phase=%s)", h.phase)
	}
	return h.session.GetLabel(ctx, targetID)
}
