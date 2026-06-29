package agent

import (
	"context"
	"fmt"
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

	stream func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	auth   func(model llm.Model) (apiKey string, headers map[string]string)

	// queues (Pi PendingMessageQueue x3)
	steer    []session.Message
	followUp []session.Message
	nextTurn []session.Message

	// single active run
	phase     Phase
	mu        sync.Mutex
	runDone   chan struct{}
	runCancel chan struct{} // closed to abort current run

	// event channel for TUI
	events chan session.Event
	externalEvents bool

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

type pendingWrite struct {
	entryType string
}

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
	StreamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error)
	Auth     func(model llm.Model) (apiKey string, headers map[string]string)

	// Compaction settings.
	Compaction    CompactionSettings
	ContextWindow int // model context window size in tokens
}

// NewHarness creates a new Harness from the given configuration.
func NewHarness(cfg HarnessConfig) *Harness {
	toolMap := make(map[string]Tool, len(cfg.Tools))
	for _, t := range cfg.Tools {
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
	return h
}

// Events returns the channel the TUI subscribes to.
func (h *Harness) Events() <-chan session.Event { return h.events }

// emit sends an event to the TUI channel.
func (h *Harness) emit(e session.Event) {
	select {
	case h.events <- e:
	default:
		// channel full; drop event (non-blocking)
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

	// Drain nextTurn queue and prepend to prompts.
	prompts := h.drainNextTurn()
	prompts = append(prompts, session.NewUserText(text, time.Now()))

	// Build tools for the loop.
	tools := h.buildTools()

	// Build LoopConfig.
	cfg := h.buildLoopConfig(ctx, tools)

	// Run the loop with overflow recovery.
	var msgs []session.Message
	for range 2 {
		msgs = RunLoop(ctx, prompts, TurnContext{
			SystemPrompt: h.sysprompt,
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
		StreamFn: h.stream,
		Convert:  DefaultConvert,
		Auth:     h.auth,
		DrainSteer: func() []session.Message {
			return h.drain(&h.steer)
		},
		DrainFollowUp: func() []session.Message {
			return h.drain(&h.followUp)
		},
		PrepareNextTurn: func(ctx context.Context) *NextTurnSnapshot {
			h.flushPending(ctx)
			snap, err := h.session.BuildContext(ctx)
			if err != nil {
				return nil
			}
			return &NextTurnSnapshot{
				Context: TurnContext{
					SystemPrompt: h.sysprompt,
					Messages:     snap.Messages,
				},
				Model:    &h.model,
				Thinking: &h.thinking,
			}
		},
	}
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
}

// FollowUp queues a message to be processed after the agent would otherwise stop.
func (h *Harness) FollowUp(text string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.followUp = append(h.followUp, session.NewUserText(text, time.Now()))
}

// NextTurn queues a message to be prepended to the next prompt.
func (h *Harness) NextTurn(text string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.nextTurn = append(h.nextTurn, session.NewUserText(text, time.Now()))
}

// --- buffered writes ---

func (h *Harness) flushPending(ctx context.Context) {
	for _, pw := range h.pending {
		h.session.AppendCustom(ctx, &session.CustomEntry{
			Type: pw.entryType,
		})
	}
	h.pending = nil
}

// SetModel changes the model. If a run is active, buffered until next turn boundary.
func (h *Harness) SetModel(model llm.Model) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.model = model
	// TODO: buffer if run is active, or persist immediately if idle
}

// SetThinking changes the thinking level.
func (h *Harness) SetThinking(level session.ThinkingLevel) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.thinking = level
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
