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
	durable   session.DurableStore
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
	steeringMode     string // "one-at-a-time" | "all"
	followUpMode     string // "one-at-a-time" | "all"
	queueCapacity    int
	maxParallelTools int

	// single active run
	phase       Phase
	closed      bool
	mu          sync.Mutex
	commandMu   sync.Mutex
	commands    chan runtimeCommand
	commandStop chan struct{}
	runDone     chan struct{}
	runCancel   chan struct{} // closed to abort current run

	// event subscription
	eventHub *eventHub
	done     chan struct{}

	// hook registry (Pi on/emitHook pattern)
	hooks map[string][]HookHandler

	// buffered session writes during a run
	pending []pendingWrite

	// activeTurnID and activeTurnLeaf identify the durable, uncommitted turn
	// owned by the current prompt. Ordinary replay cannot see these entries;
	// TurnBranch is used to build the live context until commit.
	activeTurnID   string
	activeTurnLeaf string
	turnCommitted  bool
	turnAborted    bool

	// thinkingPending prevents a live active-turn change from being overwritten
	// by a stale session snapshot and records the last durable level for rollback.
	thinkingPending     bool
	thinkingRollback    session.ThinkingLevel
	thinkingGeneration  uint64
	thinkingRollbackSet bool

	// compaction settings
	compaction     CompactionSettings
	contextWindow  int
	approvals      *ApprovalBroker
	actionBoundary ActionBoundary
	actionsEnabled bool
	closeResources []func() error
	resourcesOnce  sync.Once
	resourcesErr   error
}

var (
	_ Runtime          = (*Harness)(nil)
	_ SessionOwner     = (*Harness)(nil)
	_ EntryPersister   = (*Harness)(nil)
	_ SessionNamer     = (*Harness)(nil)
	_ SessionForker    = (*Harness)(nil)
	_ SessionNavigator = (*Harness)(nil)
	_ SessionLabels    = (*Harness)(nil)
	_ Compactor        = (*Harness)(nil)
	_ ResourceOwner    = (*Harness)(nil)
	_ ActionRecovery   = (*Harness)(nil)
)

type Phase string

// ErrQueueFull reports that a bounded runtime input queue cannot accept more
// messages. The caller can retry or surface the rejection to the user.
var ErrQueueFull = errors.New("runtime input queue is full")

const (
	PhaseIdle       Phase = "idle"
	PhaseTurn       Phase = "turn"
	PhaseCompaction Phase = "compaction"
	PhaseBranchNav  Phase = "branch_summary"
	PhaseSessionOp  Phase = "session_operation"
)

// beginExclusive reserves the controller for a non-turn operation. The phase
// and cancellation handles are changed under the state lock; the operation
// itself must run after this method returns so provider/storage I/O never runs
// while h.mu is held.
func (h *Harness) beginExclusive(phase Phase) (func(), error) {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return nil, errors.New("harness is closed")
	}
	if h.phase != PhaseIdle {
		current := h.phase
		h.mu.Unlock()
		return nil, fmt.Errorf("harness is busy (phase=%s)", current)
	}
	h.phase = phase
	h.runDone = make(chan struct{})
	h.runCancel = make(chan struct{})
	done := h.runDone
	h.mu.Unlock()

	return func() {
		h.mu.Lock()
		if h.phase == phase {
			h.phase = PhaseIdle
			h.runCancel = nil
			if h.runDone == done {
				close(done)
				h.runDone = nil
			}
		}
		h.mu.Unlock()
	}, nil
}

// pendingWrite is a buffered session mutation applied at turn boundary.
// Pi reference: agent-harness.js pendSessionWrites (line 410-435).
type pendingWrite struct {
	apply      func(ctx context.Context, s session.Session) error
	applyStore func(ctx context.Context, store session.Store) error
	applyTurn  func(ctx context.Context, store session.DurableStore, turnID, parentID string) (string, error)
	onSuccess  func()
	onFailure  func()
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
	Session         session.Session
	Store           session.Store
	Durable         session.DurableStore // optional transactional turn journal
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
	// QueueCapacity bounds each steer, follow-up, and next-turn queue. Zero uses
	// the runtime default.
	QueueCapacity int
	// MaxParallelTools bounds tool execution workers for one turn. Zero uses the
	// runtime default.
	MaxParallelTools int

	// Metrics collects runtime statistics. When nil, metrics are not recorded.
	Metrics *Metrics

	// Compaction settings.
	Compaction    CompactionSettings
	ContextWindow int // model context window size in tokens

	// ApprovalMode controls requirement-bearing tool calls. Confirm is
	// interactive only when ApprovalInteractive is true; otherwise it denies
	// requests immediately (the print-mode fail-closed behavior).
	ApprovalMode        ApprovalMode
	ApprovalInteractive bool
	// ActionJournal is required by the production host for tools that can have
	// external effects. Without it, a configured action boundary cannot issue
	// execution authority.
	ActionJournal session.ActionJournal
	// Workdir is the explicit workspace root used for action identity and path
	// canonicalization.
	Workdir string

	// CloseResources are host-created services such as external tool clients.
	// The host invokes Harness.CloseResources after Runtime.Close.
	CloseResources []func() error
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
		session:          cfg.Session,
		store:            cfg.Store,
		durable:          cfg.Durable,
		tools:            toolMap,
		active:           active,
		model:            cfg.Model,
		thinking:         cfg.Thinking,
		sysprompt:        cfg.SysPrompt,
		log:              cfg.Logger,
		metrics:          cfg.Metrics,
		promptTemplates:  cfg.PromptTemplates,
		stream:           cfg.StreamFn,
		auth:             cfg.Auth,
		transport:        cfg.Transport,
		timeout:          cfg.Timeout,
		phase:            PhaseIdle,
		commands:         make(chan runtimeCommand, controllerCommandCapacity),
		commandStop:      make(chan struct{}),
		eventHub:         newEventHub(),
		done:             make(chan struct{}),
		compaction:       cfg.Compaction,
		contextWindow:    cfg.ContextWindow,
		steeringMode:     cfg.SteeringMode,
		followUpMode:     cfg.FollowUpMode,
		queueCapacity:    cfg.QueueCapacity,
		maxParallelTools: cfg.MaxParallelTools,
	}
	if h.steeringMode == "" {
		h.steeringMode = "one-at-a-time"
	}
	if h.followUpMode == "" {
		h.followUpMode = "one-at-a-time"
	}
	if h.queueCapacity <= 0 {
		h.queueCapacity = 64
	}
	if h.maxParallelTools <= 0 {
		h.maxParallelTools = 8
	}
	if h.hooks == nil {
		h.hooks = make(map[string][]HookHandler)
	}
	h.approvals = NewApprovalBroker(cfg.ApprovalMode, cfg.ApprovalInteractive, h.emit)
	if cfg.ActionJournal != nil {
		h.actionBoundary = newJournalActionBoundary(
			cfg.ActionJournal, h.approvals, cfg.ApprovalMode,
			cfg.ApprovalInteractive, cfg.Workdir,
		)
		h.actionsEnabled = true
	} else if hasExternalActionTool(cfg.Tools) {
		// A runtime with effect-capable tools but no journal is still wired to
		// the boundary so execution fails closed instead of falling back to the
		// legacy ephemeral approval path.
		h.actionBoundary = newJournalActionBoundary(
			nil, h.approvals, cfg.ApprovalMode, cfg.ApprovalInteractive, cfg.Workdir,
		)
	}
	h.closeResources = append([]func() error(nil), cfg.CloseResources...)
	go h.commandLoop()
	return h
}

func hasExternalActionTool(tools []Tool) bool {
	for _, tool := range tools {
		if tool.RequiresAction || tool.ApprovalRequirement != nil {
			return true
		}
	}
	return false
}

// UnsettledActions returns durable action records that need completion or
// explicit reconciliation. Indeterminate records remain visible after restart.
func (h *Harness) UnsettledActions(ctx context.Context) ([]session.ActionRecord, error) {
	journal, ok := h.store.(session.ActionJournal)
	if !ok {
		return nil, errors.New("session store does not support action recovery")
	}
	return journal.UnsettledActions(ctx)
}

// ReconcileAction completes an indeterminate action only with explicit
// verification evidence from the host or an executor-specific verifier.
func (h *Harness) ReconcileAction(ctx context.Context, actionID string, state session.ActionState, verification, resultIdentity, reason, cleanup string) (session.ActionRecord, error) {
	journal, ok := h.store.(session.ActionJournal)
	if !ok {
		return session.ActionRecord{}, errors.New("session store does not support action recovery")
	}
	return journal.ReconcileAction(ctx, actionID, state, verification, resultIdentity, reason, cleanup)
}

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
	var hookErrors []error
	for _, fn := range snapshot {
		patch, fnErr := fn(payload)
		if fnErr != nil {
			hookErrors = append(hookErrors, fnErr)
			continue
		}
		if patch != nil {
			patches = append(patches, patch)
		}
	}
	return patches, errors.Join(hookErrors...)
}

// emit publishes an event through the controller-owned bounded subscription
// hub. Subscribers never execute on the publisher and cannot block it.
func (h *Harness) emit(e session.Event) {
	if h.eventHub != nil {
		h.eventHub.publish(e)
	}
}

// emitLocked publishes an event while h.mu is already held. Publication is
// non-blocking and has no callback reentry path.
func (h *Harness) emitLocked(e session.Event) {
	if h.eventHub != nil {
		h.eventHub.publish(e)
	}
}

func newUserMessage(text string, images []session.ImageContent, timestamp time.Time) *session.UserMessage {
	content := make([]session.Content, 0, 1+len(images))
	content = append(content, session.TextContent{Text: text})
	for _, image := range images {
		content = append(content, image)
	}
	return &session.UserMessage{Content: content, Timestamp: timestamp}
}

func cloneImageContents(images []session.ImageContent) []session.ImageContent {
	if len(images) == 0 {
		return nil
	}
	cloned := make([]session.ImageContent, len(images))
	for i, image := range images {
		cloned[i] = session.ImageContent{
			Data:     append([]byte(nil), image.Data...),
			MimeType: image.MimeType,
		}
	}
	return cloned
}

// Prompt submits a user message and runs the agent turn.
// Returns the final assistant message. Blocks until the turn completes.
//
// Reference: Pi agent-harness.js prompt (line 541).
func (h *Harness) Prompt(ctx context.Context, text string, images ...session.ImageContent) (session.Message, error) {
	return h.submitPrompt(ctx, text, images)
}

// runPrompt executes an accepted turn. Acceptance and phase reservation are
// performed by the controller command loop before this worker starts.
func (h *Harness) runPrompt(ctx context.Context, text string, images ...session.ImageContent) (session.Message, error) {
	turnStart := time.Now()
	promptImages := cloneImageContents(images)
	h.mu.Lock()
	cancel := h.runCancel
	h.mu.Unlock()

	defer func() {
		dur := time.Since(turnStart)
		h.mu.Lock()
		activeTurnID := h.activeTurnID
		turnCommitted := h.turnCommitted
		turnAborted := h.turnAborted
		h.mu.Unlock()
		if activeTurnID != "" && !turnCommitted && !turnAborted && h.durable != nil {
			if err := h.durable.AbortTurn(context.Background(), activeTurnID, "turn ended before durable commit"); err != nil {
				h.logf(slog.LevelError, "abort uncommitted turn failed", slog.String("turn_id", activeTurnID), slog.String("error", err.Error()))
			}
		}
		if activeTurnID != "" && !turnCommitted {
			h.discardAbortedTurnWrites()
		}
		h.mu.Lock()
		if h.activeTurnID == activeTurnID {
			h.activeTurnID = ""
			h.activeTurnLeaf = ""
			h.turnCommitted = false
			h.turnAborted = false
		}
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

	// A retained thinking write must be durable before rebuilding the context.
	// Other deferred setters retain their turn-boundary SavePoint semantics.
	h.mu.Lock()
	thinkingPending := h.thinkingPending
	h.mu.Unlock()
	if thinkingPending {
		if err := h.flushPending(ctx); err != nil {
			return nil, fmt.Errorf("flush pending writes: %w", err)
		}
	}

	// Build turn context from session.
	snap, err := h.session.BuildContext(ctx)
	if err != nil {
		return nil, fmt.Errorf("build context: %w", err)
	}

	// Begin the durable turn before hooks or provider work. The accepted input
	// and context leaf are recovery evidence even if setup, streaming, or
	// cancellation prevents a commit.
	if h.durable != nil {
		turn, err := h.durable.BeginTurn(ctx, session.NewEntryID(), text, h.session.GetLeafID())
		if err != nil {
			return nil, fmt.Errorf("begin durable turn: %w", err)
		}
		h.mu.Lock()
		h.activeTurnID = turn.ID
		h.activeTurnLeaf = turn.LeafID
		h.turnCommitted = false
		h.turnAborted = false
		h.mu.Unlock()
	}

	// Restore active state from session tree (survives replay).
	// If the session has recorded model/thinking/tool changes, they take precedence
	// over the harness's current state (which may be from a fresh start).
	h.mu.Lock()
	if snap.ActiveModel != "" && snap.ActiveModel != h.model.ID {
		// Model was changed mid-session — the session tree is authoritative.
		// The harness's model at construction time was a starting point.
	}
	if snap.Thinking != "" && !h.thinkingPending {
		h.thinking = snap.Thinking
	}
	if len(snap.ActiveTools) > 0 {
		h.active = snap.ActiveTools
	}
	h.mu.Unlock()

	// Drain nextTurn queue and prepend to prompts.
	prompts := h.drainNextTurn() // holds its own lock
	prompts = append(prompts, newUserMessage(text, promptImages, time.Now()))

	// Emit before_agent_start hook — inject extra messages.
	patches, err := h.emitHook(HookBeforeAgentStart, beforeAgentStartPayload{
		Prompt:       text,
		Images:       cloneImageContents(promptImages),
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
		if compactErr := h.compactAfterTurn(ctx); compactErr != nil {
			break // can't compact, give up
		}
		snap, err = h.contextSnapshot(ctx)
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

	// Return the last assistant message. A terminal failure is not a successful
	// Prompt merely because the loop produced an error message; callers need the
	// durable outcome to distinguish committed work from an aborted/failed turn.
	var assistant *session.AssistantMessage
	for i := len(msgs) - 1; i >= 0; i-- {
		if am, ok := msgs[i].(*session.AssistantMessage); ok {
			assistant = am
			break
		}
	}
	if assistant == nil {
		return nil, fmt.Errorf("no assistant message produced")
	}
	if reason := terminalTurnFailure(msgs); reason != "" {
		h.mu.Lock()
		turnID := h.activeTurnID
		h.mu.Unlock()
		return assistant, &TurnError{
			Outcome: turnOutcomeForMessages(msgs),
			TurnID:  turnID,
			Err:     errors.New(reason),
		}
	}
	return assistant, nil
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
		if err := h.persistMessage(ctx, e.Message); err != nil {
			err = fmt.Errorf("persist message: %w", err)
			h.logf(slog.LevelError, "persist message failed", slog.String("error", err.Error()))
			return err
		}
		h.logMessage(e.Message)
		h.emit(e)

	case session.TurnEnd:
		h.mu.Lock()
		hadPending := len(h.pending) > 0
		h.mu.Unlock()
		if err := h.flushPending(ctx); err != nil {
			return err
		}
		// Emit SavePoint after durable writes (Pi: line ~480). HadPendingMutations
		// must be captured BEFORE flush, otherwise always false.
		h.emit(session.SavePoint{HadPendingMutations: hadPending})
		// Auto-compaction check after turn ends.
		if h.canCompactAfterTurn() && ShouldCompactAfterTurn(ctx, h.session, h.contextWindow, h.compaction) {
			if err := h.compact(ctx); err != nil {
				h.emit(&session.Error{Err: fmt.Errorf("auto-compact: %w", err)})
			}
		}
		// Forward TurnEnd to the TUI so it can call handleTurnFinished.
		h.emit(e)

	case session.AgentEnd:
		if err := h.flushPending(ctx); err != nil {
			return err
		}
		if reason := terminalTurnFailure(e.Messages); reason != "" {
			if err := h.abortTurn(ctx, reason); err != nil {
				return err
			}
		} else if err := h.commitTurn(ctx); err != nil {
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

// persistMessage appends conversation messages to the active durable turn.
// Non-SQLite stores retain the existing façade path until their runtime
// storage contract is migrated; production SQLite always takes the turn path.
func (h *Harness) persistMessage(ctx context.Context, msg session.Message) error {
	h.mu.Lock()
	turnID := h.activeTurnID
	parentID := h.activeTurnLeaf
	durable := h.durable
	h.mu.Unlock()
	if durable == nil || turnID == "" {
		_, err := h.session.AppendMessage(ctx, msg)
		return err
	}
	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{
			ID:        session.NewEntryID(),
			ParentID:  parentID,
			Timestamp: time.Now(),
		},
		Message: msg,
	}
	id, err := durable.AppendTurnEntry(ctx, turnID, entry)
	if err != nil {
		return err
	}
	h.mu.Lock()
	if h.activeTurnID == turnID {
		h.activeTurnLeaf = id
	}
	h.mu.Unlock()
	return nil
}

// contextSnapshot returns the committed session context, or the active turn
// projection when the current prompt has staged entries that are not replayable
// yet.
func (h *Harness) contextSnapshot(ctx context.Context) (session.ContextSnapshot, error) {
	h.mu.Lock()
	turnID := h.activeTurnID
	durable := h.durable
	h.mu.Unlock()
	if durable != nil && turnID != "" {
		entries, err := durable.TurnBranch(ctx, turnID)
		if err != nil {
			return session.ContextSnapshot{}, err
		}
		return session.ProjectContext(entries)
	}
	return h.session.BuildContext(ctx)
}

func (h *Harness) commitTurn(ctx context.Context) error {
	h.mu.Lock()
	turnID := h.activeTurnID
	durable := h.durable
	h.mu.Unlock()
	if durable == nil || turnID == "" {
		return nil
	}
	if err := durable.CommitTurn(ctx, turnID); err != nil {
		return fmt.Errorf("commit durable turn: %w", err)
	}
	h.mu.Lock()
	if h.activeTurnID == turnID {
		h.turnCommitted = true
	}
	h.mu.Unlock()
	return nil
}

func (h *Harness) abortTurn(ctx context.Context, reason string) error {
	h.mu.Lock()
	turnID := h.activeTurnID
	durable := h.durable
	h.mu.Unlock()
	if durable == nil || turnID == "" {
		return nil
	}
	if err := durable.AbortTurn(ctx, turnID, reason); err != nil {
		return fmt.Errorf("abort durable turn: %w", err)
	}
	h.mu.Lock()
	if h.activeTurnID == turnID {
		h.turnAborted = true
	}
	h.mu.Unlock()
	return nil
}

func (h *Harness) discardAbortedTurnWrites() {
	h.mu.Lock()
	pending := h.pending
	h.pending = nil
	h.mu.Unlock()
	for _, write := range pending {
		if write.onFailure != nil {
			write.onFailure()
		}
	}
}

func (h *Harness) canCompactAfterTurn() bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.activeTurnID == "" || h.durable == nil
}

func (h *Harness) compactAfterTurn(ctx context.Context) error {
	if !h.canCompactAfterTurn() {
		return errors.New("compaction cannot run inside an uncommitted durable turn")
	}
	return h.compact(ctx)
}

func terminalTurnFailure(messages []session.Message) string {
	for i := len(messages) - 1; i >= 0; i-- {
		assistant, ok := messages[i].(*session.AssistantMessage)
		if !ok {
			continue
		}
		if assistant.Error != "" {
			return assistant.Error
		}
		switch assistant.StopReason {
		case session.StopReasonAborted, session.StopReasonError:
			return string(assistant.StopReason)
		}
		return ""
	}
	return ""
}

func turnOutcomeForMessages(messages []session.Message) TurnOutcome {
	for i := len(messages) - 1; i >= 0; i-- {
		assistant, ok := messages[i].(*session.AssistantMessage)
		if !ok {
			continue
		}
		if assistant.StopReason == session.StopReasonAborted {
			return TurnAborted
		}
		return TurnFailed
	}
	return TurnFailed
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
	turnID := h.activeTurnID
	h.mu.Unlock()

	return LoopConfig{
		Model:            model,
		Thinking:         thinking,
		SessionID:        h.session.Meta().ID,
		TurnID:           turnID,
		Tools:            tools,
		ActionBoundary:   h.actionBoundary,
		MaxParallelTools: h.maxParallelTools,
		StreamFn:         h.wrapStreamFn(),
		Convert:          DefaultConvert,
		Auth:             h.auth,
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
			if err != nil {
				return nil
			}
			for _, p := range patches {
				if bp, ok := p.(*ToolCallDecision); ok && bp != nil && bp.Block {
					return bp
				}
			}
			if h.actionsEnabled {
				return nil
			}
			h.mu.Lock()
			registered, ok := h.tools[ctx.ToolCall.Name]
			h.mu.Unlock()
			if !ok || registered.ApprovalRequirement == nil {
				return nil
			}
			requirement, required, err := registered.ApprovalRequirement(ctx.Args)
			if err != nil {
				return &ToolCallDecision{
					Block:  true,
					Reason: fmt.Sprintf("tool approval: %v", err),
				}
			}
			if !required {
				return nil
			}
			if h.approvals == nil {
				return &ToolCallDecision{
					Block:  true,
					Reason: "tool approval is unavailable in this runtime",
				}
			}
			request := session.ApprovalRequest{
				ToolCallID: ctx.ToolCall.ID,
				ToolName:   ctx.ToolCall.Name,
				Category:   requirement.Category,
				Operation:  requirement.Operation,
				Resource:   requirement.Resource,
			}
			var outcome approvalOutcome
			if requirement.AlwaysConfirm {
				outcome = h.approvals.RequestForced(ctx.RunContext, request)
			} else {
				outcome = h.approvals.Request(ctx.RunContext, request)
			}
			if outcome.decision == session.ApprovalAllow ||
				outcome.decision == session.ApprovalAlways {
				return nil
			}
			reason := outcome.reason
			if reason == "" {
				reason = "tool call denied by user"
			}
			return &ToolCallDecision{Block: true, Reason: reason}
		},
		AfterToolCall: func(ctx ToolCallResultContext) *ToolCallPatch {
			patches, err := h.emitHook(HookToolResult, toolResultPayload{
				ToolCallID: ctx.ToolCall.ID,
				ToolName:   ctx.ToolCall.Name,
				Input:      ctx.Args,
				Content:    ctx.Result.Content,
				IsError:    ctx.Result.IsError,
			})
			if err != nil {
				return &ToolCallPatch{Error: err}
			}
			if len(patches) == 0 {
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
			snap, err := h.contextSnapshot(ctx)
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
				Tools:    h.buildTools(),
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

type cancelOnCloseStream struct {
	llm.Stream
	cancel context.CancelFunc
}

func (s *cancelOnCloseStream) Close() error {
	s.cancel()
	return s.Stream.Close()
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
		timeout := h.timeout
		transport := h.transport
		h.mu.Unlock()

		// Transport is request-scoped: start from the harness snapshot and let
		// ordered hook patches override only this request.
		req.Transport = transport

		effectiveHeaders := modelHeaders
		for k, v := range req.Headers {
			effectiveHeaders[k] = v
		}
		req.Headers = effectiveHeaders

		// Emit before_provider_request hook — allows extensions to patch request headers.
		patches, err := h.emitHook(HookBeforeProviderRequest, beforeProviderRequestPayload{
			Model:     req.Model,
			SessionID: req.SessionID,
			Headers:   effectiveHeaders,
		})
		if err != nil {
			return nil, fmt.Errorf("before_provider_request hook: %w", err)
		}

		// Apply patches to this request only. Pi's before_provider_request
		// options are per-call; they must not mutate future model requests.
		for _, p := range patches {
			if bp, ok := p.(*BeforeProviderRequestPatch); ok && bp != nil {
				for k, v := range bp.Headers {
					req.Headers[k] = v
				}
				if bp.Transport != nil {
					transport = *bp.Transport
					req.Transport = transport
				}
				if bp.Timeout != nil {
					timeout = *bp.Timeout
				}
			}
		}

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

		// Call the base stream function with the effective per-request timeout.
		streamCtx := ctx
		var cancel context.CancelFunc
		if timeout > 0 {
			streamCtx, cancel = context.WithTimeout(ctx, timeout)
		}
		streamCtx = llm.WithRetryObserver(streamCtx, func(retry llm.RetryEvent) {
			h.emit(session.ProviderRetry{
				Attempt:   retry.Attempt,
				Delay:     retry.Delay,
				Err:       retry.Err,
				Timestamp: time.Now(),
			})
		})
		stream, err := base(streamCtx, req)
		if err != nil {
			if cancel != nil {
				cancel()
			}
		} else if cancel != nil {
			stream = &cancelOnCloseStream{Stream: stream, cancel: cancel}
		}

		// Notify registered handlers after the provider responds. The hook is the
		// behavior-bearing extension point; no synthetic event is needed.
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
	Images       []session.ImageContent
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

// appendQueued appends one user-controlled message to a bounded runtime queue.
// The caller must hold h.mu. Keeping the bound at the owner makes queue
// capacity a lifecycle invariant instead of a UI convention.
func (h *Harness) appendQueued(queue *[]session.Message, message session.Message) error {
	if len(*queue) >= h.queueCapacity {
		return ErrQueueFull
	}
	*queue = append(*queue, message)
	return nil
}

// Steer queues a message to be injected before the next assistant response.
// Returns an error if the harness is idle (Pi: steer/followUp reject while idle).
func (h *Harness) steerDirect(text string, images ...session.ImageContent) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	if h.phase == PhaseIdle {
		h.mu.Unlock()
		return fmt.Errorf("cannot steer while idle")
	}
	if err := h.appendQueued(&h.steer, newUserMessage(text, cloneImageContents(images), time.Now())); err != nil {
		h.mu.Unlock()
		return fmt.Errorf("queue steer: %w", err)
	}
	steer := make([]session.Message, len(h.steer))
	copy(steer, h.steer)
	followUp := make([]session.Message, len(h.followUp))
	copy(followUp, h.followUp)
	nextTurn := make([]session.Message, len(h.nextTurn))
	copy(nextTurn, h.nextTurn)
	h.mu.Unlock()
	// emit outside lock — emit() acquires h.mu internally for listener snapshot
	h.emit(session.QueueUpdate{Steer: steer, FollowUp: followUp, NextTurn: nextTurn})
	return nil
}

// FollowUp queues a message to be processed after the agent would otherwise stop.
// Returns an error if the harness is idle.
func (h *Harness) followUpDirect(text string, images ...session.ImageContent) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	if h.phase == PhaseIdle {
		h.mu.Unlock()
		return fmt.Errorf("cannot follow up while idle")
	}
	if err := h.appendQueued(&h.followUp, newUserMessage(text, cloneImageContents(images), time.Now())); err != nil {
		h.mu.Unlock()
		return fmt.Errorf("queue follow-up: %w", err)
	}
	steer := make([]session.Message, len(h.steer))
	copy(steer, h.steer)
	followUp := make([]session.Message, len(h.followUp))
	copy(followUp, h.followUp)
	nextTurn := make([]session.Message, len(h.nextTurn))
	copy(nextTurn, h.nextTurn)
	h.mu.Unlock()
	// emit outside lock — emit() acquires h.mu internally for listener snapshot
	h.emit(session.QueueUpdate{Steer: steer, FollowUp: followUp, NextTurn: nextTurn})
	return nil
}

// NextTurn queues a message to be prepended to the next prompt.
func (h *Harness) nextTurnDirect(text string, images ...session.ImageContent) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	if err := h.appendQueued(&h.nextTurn, newUserMessage(text, cloneImageContents(images), time.Now())); err != nil {
		h.mu.Unlock()
		return fmt.Errorf("queue next turn: %w", err)
	}
	steer := make([]session.Message, len(h.steer))
	copy(steer, h.steer)
	followUp := make([]session.Message, len(h.followUp))
	copy(followUp, h.followUp)
	nextTurn := make([]session.Message, len(h.nextTurn))
	copy(nextTurn, h.nextTurn)
	h.mu.Unlock()
	// emit outside lock — emit() acquires h.mu internally for listener snapshot
	h.emit(session.QueueUpdate{Steer: steer, FollowUp: followUp, NextTurn: nextTurn})
	return nil
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
	h.emit(session.QueueUpdate{
		Steer: steer, FollowUp: followUp, NextTurn: nextTurn,
	})
}

// --- buffered writes ---

func (h *Harness) flushPending(ctx context.Context) error {
	h.mu.Lock()
	pending := h.pending
	h.pending = nil
	turnID := h.activeTurnID
	turnLeaf := h.activeTurnLeaf
	durable := h.durable
	h.mu.Unlock()
	for i, pw := range pending {
		var err error
		var writtenID string
		if durable != nil && turnID != "" {
			if pw.applyTurn == nil {
				err = errors.New("pending write has no durable turn operation")
			} else {
				writtenID, err = pw.applyTurn(ctx, durable, turnID, turnLeaf)
			}
		} else if pw.applyStore != nil {
			err = pw.applyStore(ctx, h.store)
		} else {
			err = pw.apply(ctx, h.session)
		}
		if err == nil {
			if writtenID != "" {
				turnLeaf = writtenID
				h.mu.Lock()
				if h.activeTurnID == turnID {
					h.activeTurnLeaf = writtenID
				}
				h.mu.Unlock()
			}
			if pw.onSuccess != nil {
				pw.onSuccess()
			}
			continue
		}
		if pw.onFailure != nil {
			pw.onFailure()
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

func durableEntryBase(parentID string) session.EntryBase {
	return session.EntryBase{ID: session.NewEntryID(), ParentID: parentID, Timestamp: time.Now()}
}

func appendDurableEntry(ctx context.Context, store session.DurableStore, turnID, parentID string, entry session.Entry) (string, error) {
	if entry == nil {
		return "", errors.New("durable entry is nil")
	}
	if entry.ParentID() != parentID {
		return "", fmt.Errorf("durable entry %q parent %q does not match active leaf %q", entry.ID(), entry.ParentID(), parentID)
	}
	return store.AppendTurnEntry(ctx, turnID, entry)
}

func reparentDurableEntry(entry session.Entry, parentID string) (session.Entry, error) {
	if entry == nil {
		return nil, errors.New("durable entry is nil")
	}
	if entry.ParentID() == parentID {
		return entry, nil
	}
	if entry.ParentID() != "" {
		return nil, fmt.Errorf("entry %q already has parent %q", entry.ID(), entry.ParentID())
	}
	if custom, ok := entry.(*session.CustomEntry); ok {
		copy := *custom
		copy.EntryBase.ParentID = parentID
		return &copy, nil
	}
	return nil, fmt.Errorf("cannot attach %T to active durable turn without an explicit parent", entry)
}

// SetModel changes the model. If a run is active, buffered until next turn boundary.
func (h *Harness) setModelDirect(model llm.Model) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	oldModel := h.model
	h.model = model
	if model.Provider != oldModel.Provider || model.ID != oldModel.ID {
		h.pending = append(h.pending, pendingWrite{
			apply: func(ctx context.Context, s session.Session) error {
				_, err := s.AppendModelChange(ctx, model.Provider, model.ID)
				return err
			},
			applyTurn: func(ctx context.Context, d session.DurableStore, turnID, parentID string) (string, error) {
				return appendDurableEntry(ctx, d, turnID, parentID, &session.ModelChangeEntry{
					EntryBase: durableEntryBase(parentID), Provider: model.Provider, ModelID: model.ID,
				})
			},
		})
	}
	h.emitLocked(session.ModelUpdate{
		Model:    model.ID,
		Previous: oldModel.ID,
		Source:   session.UpdateSourceSet,
	})
	h.mu.Unlock()
	return nil
}

// SetThinking changes the thinking level. Idle changes are durable before the
// live value changes so the next Prompt cannot restore the previous tree value.
// Active changes are buffered for the next turn boundary.
func (h *Harness) setThinkingDirect(ctx context.Context, level session.ThinkingLevel) error {
	if ctx == nil {
		ctx = context.Background()
	}
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	idle := h.phase == PhaseIdle
	sess := h.session
	h.mu.Unlock()
	if idle {
		if sess == nil {
			return errors.New("harness has no session")
		}
		finish, err := h.beginExclusive(PhaseSessionOp)
		if err != nil {
			return err
		}
		if _, err := sess.AppendThinkingLevelChange(ctx, level); err != nil {
			finish()
			return fmt.Errorf("persist thinking level: %w", err)
		}
		h.mu.Lock()
		previous := h.thinking
		h.thinking = level
		h.emitLocked(session.ThinkingUpdate{Level: level, Previous: previous})
		h.mu.Unlock()
		finish()
		return nil
	}

	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	previous := h.thinking
	h.thinking = level
	if h.phase != PhaseIdle {
		h.thinkingGeneration++
		generation := h.thinkingGeneration
		if !h.thinkingPending {
			h.thinkingRollback = previous
			h.thinkingRollbackSet = true
		}
		h.thinkingPending = true
		h.pending = append(h.pending, pendingWrite{
			apply: func(ctx context.Context, s session.Session) error {
				_, err := s.AppendThinkingLevelChange(ctx, level)
				return err
			},
			applyTurn: func(ctx context.Context, d session.DurableStore, turnID, parentID string) (string, error) {
				return appendDurableEntry(ctx, d, turnID, parentID, &session.ThinkingChangeEntry{
					EntryBase: durableEntryBase(parentID), Level: level,
				})
			},
			onSuccess: func() {
				h.mu.Lock()
				if generation == h.thinkingGeneration {
					h.thinking = level
					h.thinkingPending = false
					h.thinkingRollbackSet = false
				} else {
					h.thinkingRollback = level
				}
				h.mu.Unlock()
			},
			onFailure: func() {
				h.mu.Lock()
				if !h.thinkingRollbackSet {
					h.mu.Unlock()
					return
				}
				rollback := h.thinkingRollback
				previous := h.thinking
				h.thinking = rollback
				h.mu.Unlock()
				if previous != rollback {
					h.emit(session.ThinkingUpdate{Level: rollback, Previous: previous})
				}
			},
		})
	}
	h.emitLocked(session.ThinkingUpdate{Level: level, Previous: previous})
	h.mu.Unlock()
	return nil
}

// SetTools changes the active tools.
func (h *Harness) setToolsDirect(tools []Tool, active []string) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	toolMap := make(map[string]Tool, len(tools))
	for _, t := range tools {
		toolMap[t.Name] = t
	}
	for _, name := range active {
		if _, ok := toolMap[name]; !ok {
			h.mu.Unlock()
			return fmt.Errorf("tool not found: %s", name)
		}
	}
	previous := append([]string(nil), h.active...)
	h.tools = toolMap
	h.active = append([]string(nil), active...)
	persistActive := append([]string(nil), active...)
	eventActive := append([]string(nil), active...)
	h.pending = append(h.pending, pendingWrite{
		apply: func(ctx context.Context, s session.Session) error {
			_, err := s.AppendActiveToolsChange(ctx, persistActive)
			return err
		},
		applyTurn: func(ctx context.Context, d session.DurableStore, turnID, parentID string) (string, error) {
			return appendDurableEntry(ctx, d, turnID, parentID, &session.ToolsChangeEntry{
				EntryBase: durableEntryBase(parentID), ActiveTools: append([]string(nil), persistActive...),
			})
		},
	})
	h.emitLocked(session.ToolsUpdate{Active: eventActive, Previous: previous})
	h.mu.Unlock()
	return nil
}

// ActivateTools adds registered tools to the active set without replacing the
// registry or already-active tools. It is intentionally harness-owned: the
// registry defines tools, while the harness controls provider visibility and
// session persistence.
func (h *Harness) activateToolsDirect(ctx context.Context, names []string) error {
	if ctx == nil {
		ctx = context.Background()
	}
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}

	activeSet := make(map[string]struct{}, len(h.active)+len(names))
	for _, name := range h.active {
		activeSet[name] = struct{}{}
	}
	for _, name := range names {
		if _, ok := h.tools[name]; !ok {
			h.mu.Unlock()
			return fmt.Errorf("tool not found: %s", name)
		}
	}

	updated := append([]string(nil), h.active...)
	for _, name := range names {
		if _, ok := activeSet[name]; ok {
			continue
		}
		activeSet[name] = struct{}{}
		updated = append(updated, name)
	}
	if len(updated) == len(h.active) {
		h.mu.Unlock()
		return nil
	}

	previous := append([]string(nil), h.active...)
	if h.phase != PhaseIdle {
		persistActive := append([]string(nil), updated...)
		h.pending = append(h.pending, pendingWrite{
			apply: func(ctx context.Context, s session.Session) error {
				_, err := s.AppendActiveToolsChange(ctx, persistActive)
				return err
			},
			applyTurn: func(ctx context.Context, d session.DurableStore, turnID, parentID string) (string, error) {
				return appendDurableEntry(ctx, d, turnID, parentID, &session.ToolsChangeEntry{
					EntryBase: durableEntryBase(parentID), ActiveTools: append([]string(nil), persistActive...),
				})
			},
		})
		h.active = updated
		h.emitLocked(session.ToolsUpdate{
			Active:   append([]string(nil), updated...),
			Previous: previous,
		})
		h.mu.Unlock()
		return nil
	}
	sess := h.session
	h.mu.Unlock()

	if sess == nil {
		return errors.New("harness has no session")
	}
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return err
	}
	if _, err := sess.AppendActiveToolsChange(ctx, updated); err != nil {
		finish()
		return fmt.Errorf("persist active tools: %w", err)
	}
	h.mu.Lock()
	h.active = updated
	h.emitLocked(session.ToolsUpdate{
		Active:   append([]string(nil), updated...),
		Previous: previous,
	})
	h.mu.Unlock()
	finish()
	return nil
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

// ResolveApproval supplies the host's decision for one pending tool call.
// It is intentionally a small optional runner capability used by the TUI.
func (h *Harness) ResolveApproval(id string, decision session.ApprovalDecision) error {
	if h == nil || h.approvals == nil {
		return errors.New("approval broker is unavailable")
	}
	return h.approvals.Resolve(id, decision)
}

// Close releases resources. Active work is cancelled before waiting for its
// completion so providers and tools that honor the run signal can terminate.
func (h *Harness) Close() error {
	h.commandMu.Lock()
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		h.commandMu.Unlock()
		return nil
	}
	h.closed = true
	close(h.commandStop)
	close(h.done)
	cancel := h.runCancel
	h.mu.Unlock()
	h.commandMu.Unlock()

	if cancel != nil {
		select {
		case <-cancel:
		default:
			close(cancel)
		}
	}
	if h.approvals != nil {
		_ = h.approvals.Close()
	}
	// Ensure no active run is enqueueing before cancelling the dispatcher.
	h.WaitForIdle()
	flushErr := h.flushPending(context.Background())
	if h.eventHub != nil {
		h.eventHub.close()
	}
	return flushErr
}

// CloseResources releases host-created runtime services after the controller
// has stopped. It is separate from Close because the host owns the final
// resource boundary and may close shared storage independently.
func (h *Harness) CloseResources() error {
	h.resourcesOnce.Do(func() {
		for i := len(h.closeResources) - 1; i >= 0; i-- {
			h.resourcesErr = errors.Join(h.resourcesErr, h.closeResources[i]())
		}
	})
	return h.resourcesErr
}

// Shutdown attempts a graceful stop: abort any running turn, wait for
// completion (up to the context deadline), flush pending writes, and stop the
// controller. The host must call CloseResources after Shutdown/Close.
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
func (h *Harness) exportSessionBundleDirect(ctx context.Context, sessionID string) (ionexport.SessionBundle, error) {
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return ionexport.SessionBundle{}, err
	}
	defer finish()
	if h.store == nil {
		return ionexport.SessionBundle{}, errors.New("session store does not support export")
	}
	return ionexport.ExportSessionBundle(ctx, h.store, sessionID)
}

// ImportSessionBundle performs explicit transport through the harness owner.
func (h *Harness) importSessionBundleDirect(ctx context.Context, bundle ionexport.SessionBundle) (string, error) {
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return "", err
	}
	defer finish()
	if h.store == nil {
		return "", errors.New("session store does not support import")
	}
	return ionexport.ImportSessionBundle(ctx, h.store, bundle)
}

// ForkSession copies a branch into an independent durable session. The
// harness owns the idle gate so the active turn cannot race the tree copy.
func (h *Harness) forkSessionDirect(ctx context.Context, sourceID string) (string, error) {
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return "", err
	}
	defer finish()
	if h.store == nil {
		return "", errors.New("harness has no session store")
	}
	return ionexport.ForkSession(ctx, h.store, sourceID)
}

// Metrics returns the runtime metrics collector (may be nil).
func (h *Harness) Metrics() *Metrics { return h.metrics }

// Compact triggers context compaction while reserving the controller for the
// operation. The internal compact method is used by an active turn only after
// its caller has already established the lifecycle phase.
func (h *Harness) compactDirect(ctx context.Context) error {
	finish, err := h.beginExclusive(PhaseCompaction)
	if err != nil {
		return err
	}
	defer finish()
	return h.compact(ctx)
}

func (h *Harness) compact(ctx context.Context) error {
	start := time.Now()
	// Build auth from harness config.
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	model := h.model
	thinking := h.thinking
	h.mu.Unlock()
	h.logf(slog.LevelInfo, "compact start", slog.String("model", model.ID))

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
func (h *Harness) appendMessageDirect(ctx context.Context, msg session.Message) error {
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return err
	}
	defer finish()
	_, err = h.session.AppendMessage(ctx, msg)
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
func (h *Harness) persistEntryDirect(ctx context.Context, entry session.Entry) error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return errors.New("harness is closed")
	}
	if h.store == nil {
		h.mu.Unlock()
		return errors.New("harness has no session store")
	}
	if h.phase != PhaseIdle {
		h.pending = append(h.pending, pendingWrite{
			applyStore: func(ctx context.Context, store session.Store) error {
				return appendRunnerEntry(ctx, store, entry)
			},
			applyTurn: func(ctx context.Context, store session.DurableStore, turnID, parentID string) (string, error) {
				attached, err := reparentDurableEntry(entry, parentID)
				if err != nil {
					return "", err
				}
				return appendDurableEntry(ctx, store, turnID, parentID, attached)
			},
		})
		h.mu.Unlock()
		return nil
	}
	store := h.store
	h.mu.Unlock()
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return err
	}
	defer finish()
	return appendRunnerEntry(ctx, store, entry)
}

// AppendSessionInfo persists the session display name.
func (h *Harness) appendSessionInfoDirect(ctx context.Context, name string) (string, error) {
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return "", err
	}
	defer finish()
	return h.session.AppendSessionInfo(ctx, name)
}

// AppendLabel attaches a label to a target entry.
func (h *Harness) appendLabelDirect(ctx context.Context, targetID, label string) (string, error) {
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return "", err
	}
	defer finish()
	return h.session.AppendLabel(ctx, targetID, label)
}

// GetLabel returns the most recent label for a target entry.
func (h *Harness) getLabelDirect(ctx context.Context, targetID string) (string, error) {
	finish, err := h.beginExclusive(PhaseSessionOp)
	if err != nil {
		return "", err
	}
	defer finish()
	return h.session.GetLabel(ctx, targetID)
}
