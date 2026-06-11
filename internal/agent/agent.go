package agent

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Agent is the core agent loop primitive. It manages the lifecycle of an
// agent session: submit → stream → tool calls → results → done.
type Agent struct {
	config    AgentConfig
	state     AgentState
	listeners []func(session.AgentEvent)
	mu        sync.RWMutex

	// Session state (from SessionAdapter)
	id            string
	events        chan session.AgentEvent
	closed        bool
	closeOnce     sync.Once
	steeringQueue []string
	followUpQueue []string
	turnCtx       context.Context
	cancel        context.CancelFunc

	// Recovery state
	overflowAttempted bool
	retryAttempt      int
	contextTokens     int

	// Session persistence
	store session.SessionStore
	sess  session.SessionHandle
}

// New creates a new Agent with the given configuration.
func New(config AgentConfig) *Agent {
	id := config.ID
	if id == "" {
		id = "default"
	}
	return &Agent{
		config: config,
		state: AgentState{
			Model:         config.Model,
			ThinkingLevel: config.ThinkingLevel,
			SystemPrompt:  config.SystemPrompt,
			Tools:         config.Tools,
		},
		id:     id,
		events: make(chan session.AgentEvent, 100),
	}
}

func (a *Agent) emit(ev session.AgentEvent) {
	a.mu.RLock()
	onEvent := a.config.OnEvent
	a.mu.RUnlock()
	if onEvent != nil {
		onEvent(ev)
	}
}

func (a *Agent) emitInputMessage(message AgentMessage) {
	if message.Role != "user" {
		return
	}
	a.emit(session.UserMessage{
		Base:    session.BaseNow(),
		Message: message.Content,
	})
}

// toSessionAgentMessages converts domain AgentMessages to session AgentMessages
// for event payloads (TurnEnd, AgentEnd).
func toSessionAgentMessages(msgs []AgentMessage) []session.AgentMessage {
	if len(msgs) == 0 {
		return nil
	}
	sm := make([]session.AgentMessage, len(msgs))
	for i, m := range msgs {
		sm[i] = session.AgentMessage{
			Message:      m.Content,
			Reasoning:    m.Reasoning,
			InputTokens:  m.InputTokens,
			OutputTokens: m.OutputTokens,
			TotalTokens:  m.TotalTokens,
			Cost:         m.Cost,
		}
	}
	return sm
}

// State returns a copy of the current agent state.
func (a *Agent) State() AgentState {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.state
}

// SetSystemPrompt sets the system prompt for the agent.
func (a *Agent) SetSystemPrompt(prompt string) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.SystemPrompt = prompt
}

// SetTools sets the available tools for the agent.
func (a *Agent) SetTools(tools []AgentTool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.Tools = tools
}

// SetModel sets the model for the agent.
func (a *Agent) SetModel(model llm.Model) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.Model = model
	a.config.Model = model
}

// SetThinkingLevel sets the thinking level for the agent.
func (a *Agent) SetThinkingLevel(level ThinkingLevel) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.ThinkingLevel = level
	a.config.ThinkingLevel = level
}

// SetMessages replaces the provider-visible conversation history.
func (a *Agent) SetMessages(messages []AgentMessage) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.Messages = cloneAgentMessages(messages)
}

// Run starts the agent loop with the given prompt messages.
// It returns the new messages added during the run.
// Emits AgentStart at the beginning. Caller owns AgentEnd.
// Emits TurnEnd per-turn inside runLoop (Pi parity).
func (a *Agent) Run(ctx context.Context, prompts []AgentMessage) ([]AgentMessage, error) {
	a.emit(session.AgentStart{Base: session.BaseNow()})

	newMessages, err := a.acceptPrompts(ctx, prompts)
	if err != nil {
		a.mu.Lock()
		a.state.ErrorMessage = err.Error()
		a.mu.Unlock()
		// acceptPrompts error means the turn never started in runLoop.
		// Emit TurnEnd so the TUI knows the turn is over.
		a.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
		return newMessages, err
	}

	newMessages, runErr := a.execute(ctx, &newMessages)
	return newMessages, runErr
}

// Continue continues the agent loop without adding new messages.
// Used for retries — context already has user message or tool results.
// Emits AgentStart at the beginning. Caller owns AgentEnd.
func (a *Agent) Continue(ctx context.Context) ([]AgentMessage, error) {
	a.emit(session.AgentStart{Base: session.BaseNow()})

	a.mu.RLock()
	if len(a.state.Messages) == 0 {
		a.mu.RUnlock()
		err := fmt.Errorf("cannot continue: no messages in context")
		a.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
		return nil, err
	}
	lastMsg := a.state.Messages[len(a.state.Messages)-1]
	a.mu.RUnlock()

	if lastMsg.Role == "assistant" {
		err := fmt.Errorf("cannot continue from message role: assistant")
		a.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
		return nil, err
	}

	newMessages, runErr := a.execute(ctx, new([]AgentMessage))
	return newMessages, runErr
}

// execute runs the main loop with streaming state management.
// Shared by Run and Continue. Does NOT emit lifecycle events (AgentStart/AgentEnd)
// — callers own those. Emits TurnEnd per-turn inside runLoop.
func (a *Agent) execute(ctx context.Context, newMessages *[]AgentMessage) ([]AgentMessage, error) {
	a.mu.Lock()
	a.state.IsStreaming = true
	a.state.ErrorMessage = ""
	a.mu.Unlock()

	defer func() {
		a.mu.Lock()
		a.state.IsStreaming = false
		a.mu.Unlock()
	}()

	runErr := a.runLoop(ctx, newMessages)
	if runErr != nil {
		a.mu.Lock()
		a.state.ErrorMessage = runErr.Error()
		a.mu.Unlock()
	}
	return *newMessages, runErr
}

func (a *Agent) acceptPrompts(
	ctx context.Context,
	prompts []AgentMessage,
) ([]AgentMessage, error) {
	a.mu.Lock()
	a.state.Messages = append(a.state.Messages, prompts...)
	a.mu.Unlock()
	for _, prompt := range prompts {
		a.emitInputMessage(prompt)
		if err := a.writeModelMessage(ctx, agentMessageToLLM(prompt)); err != nil {
			return nil, err
		}
	}
	newMessages := make([]AgentMessage, len(prompts))
	copy(newMessages, prompts)
	return newMessages, nil
}

// getSteeringMessages returns steering messages from the config hook.
func (a *Agent) getSteeringMessages() []AgentMessage {
	a.mu.RLock()
	config := a.config
	a.mu.RUnlock()

	if config.GetSteeringMessages != nil {
		return config.GetSteeringMessages()
	}
	return nil
}

// getFollowUpMessages returns follow-up messages from the config hook.
func (a *Agent) getFollowUpMessages() []AgentMessage {
	a.mu.RLock()
	config := a.config
	a.mu.RUnlock()

	if config.GetFollowUpMessages != nil {
		return config.GetFollowUpMessages()
	}
	return nil
}

// buildContext builds the current AgentContext from the agent state.
func (a *Agent) buildContext() AgentContext {
	a.mu.RLock()
	defer a.mu.RUnlock()

	return AgentContext{
		Messages:      a.state.Messages,
		SystemPrompt:  a.state.SystemPrompt,
		Tools:         a.state.Tools,
		Model:         a.state.Model,
		ThinkingLevel: a.state.ThinkingLevel,
	}
}

func (a *Agent) applyTurnUpdate(update *AgentLoopTurnUpdate) {
	if update == nil {
		return
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	if update.Context != nil {
		a.state.Messages = cloneAgentMessages(update.Context.Messages)
		a.state.SystemPrompt = update.Context.SystemPrompt
		a.state.Tools = append([]AgentTool(nil), update.Context.Tools...)
		a.state.Model = update.Context.Model
		a.state.ThinkingLevel = update.Context.ThinkingLevel
		a.config.Model = update.Context.Model
		a.config.ThinkingLevel = update.Context.ThinkingLevel
	}
	if update.Model != nil {
		a.state.Model = *update.Model
		a.config.Model = *update.Model
	}
	if update.ThinkingLevel != nil {
		a.state.ThinkingLevel = *update.ThinkingLevel
		a.config.ThinkingLevel = *update.ThinkingLevel
	}
}

func (a *Agent) writeModelMessage(ctx context.Context, message llm.Message) error {
	a.mu.RLock()
	write := a.config.OnModelMessage
	a.mu.RUnlock()
	if write == nil {
		return nil
	}
	if isEmptyModelMessage(message) {
		return nil
	}
	if err := write(ctx, message); err != nil {
		return fmt.Errorf("persist model message: %w", err)
	}
	return nil
}

// Open initializes or creates a new session.
func (a *Agent) Open(ctx context.Context) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return fmt.Errorf("session is closed")
	}

	return nil
}

// Resume loads an existing session.
func (a *Agent) Resume(ctx context.Context, sessionID string) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return fmt.Errorf("session is closed")
	}

	a.id = sessionID

	if history, err := a.loadModelHistoryLocked(ctx); err != nil {
		return err
	} else if history != nil {
		a.SetMessages(history)
	}

	return nil
}

// CancelTurn interrupts an in-flight turn if the backend supports it.
func (a *Agent) CancelTurn(ctx context.Context) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return fmt.Errorf("session is closed")
	}

	if a.cancel != nil {
		a.cancel()
	}
	return nil
}

// Close terminates the session and cleans up resources.
func (a *Agent) Close() error {
	a.closeOnce.Do(func() {
		a.mu.Lock()
		a.closed = true
		if a.cancel != nil {
			a.cancel()
		}
		a.mu.Unlock()
		close(a.events)
	})
	return nil
}

// Events returns a read-only channel of typed events emitted by the session.
func (a *Agent) Events() <-chan session.AgentEvent {
	return a.events
}

// ID returns the session identifier.
func (a *Agent) ID() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.id
}

// Meta returns session metadata.
func (a *Agent) Meta() map[string]string {
	a.mu.Lock()
	defer a.mu.Unlock()

	return map[string]string{
		"backend": "agent",
		"model":   a.config.Model.ID,
	}
}

// SetStore sets the storage store.
func (a *Agent) SetStore(store session.SessionStore) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.store = store
}

// SetSession sets the storage session.
func (a *Agent) SetSession(sess session.SessionHandle) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.sess = sess
}

// SteerTurn sends steering input during an active turn.
func (a *Agent) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return session.SteeringResult{}, fmt.Errorf("session is closed")
	}
	a.steeringQueue = append(a.steeringQueue, text)
	a.emitQueueUpdatedLocked()
	a.mu.Unlock()

	return session.SteeringResult{
		Outcome: session.SteeringAccepted,
		Notice:  "Steering input accepted",
	}, nil
}

// FollowUpTurn sends follow-up input after the agent would stop.
func (a *Agent) FollowUpTurn(
	ctx context.Context,
	text string,
) (session.QueuedInputResult, error) {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return session.QueuedInputResult{}, fmt.Errorf("session is closed")
	}
	a.followUpQueue = append(a.followUpQueue, text)
	a.emitQueueUpdatedLocked()
	a.mu.Unlock()

	return session.QueuedInputResult{
		Outcome: session.QueuedInputAccepted,
		Notice:  "Follow-up input accepted",
	}, nil
}

// ClearQueuedInput clears queued input and returns the snapshot.
func (a *Agent) ClearQueuedInput(
	ctx context.Context,
) (session.QueuedInputSnapshot, error) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return session.QueuedInputSnapshot{}, fmt.Errorf("session is closed")
	}

	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), a.steeringQueue...),
		FollowUp: append([]string(nil), a.followUpQueue...),
	}

	a.steeringQueue = nil
	a.followUpQueue = nil
	a.emitQueueUpdatedLocked()

	return snapshot, nil
}

func (a *Agent) emitQueueUpdatedLocked() {
	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), a.steeringQueue...),
		FollowUp: append([]string(nil), a.followUpQueue...),
	}
	a.emit(session.QueuedInputUpdate{
		Base:     session.BaseNow(),
		Snapshot: snapshot,
	})
}

// handlePostAgentRun handles post-agent-run logic including overflow
// recovery and auto-retry with exponential backoff.
// Emits AgentEnd when all recovery is exhausted (Pi parity).
func (a *Agent) handlePostAgentRun(ctx context.Context, err error, newMessages []AgentMessage) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return
	}

	// Convert domain messages to session messages for event payloads.
	sessionMsgs := toSessionAgentMessages(newMessages)

	// Success or cancellation — agent already emitted TurnEnd
	if err == nil || errors.Is(err, context.Canceled) {
		a.retryAttempt = 0
		a.emit(session.AgentEnd{Base: session.BaseNow(), Messages: sessionMsgs})
		return
	}

	errMsg := err.Error()
	agentErr := NewAgentError(errMsg, err)

	// Overflow recovery: compact and retry once
	if agentErr.Code == ErrCodeOverflow && !a.overflowAttempted {
		a.overflowAttempted = true
		if a.recoverFromOverflow(ctx) {
			return
		}
	}

	// Transient error retry with exponential backoff
	if agentErr.IsRetryable && a.retryAttempt < a.config.GetMaxRetries() {
		if a.retryWithBackoff(ctx, errMsg) {
			return
		}
	}

	// Non-retryable error — agent already emitted TurnEnd{Error}
	a.emit(session.AgentEnd{Base: session.BaseNow(), Error: err, Messages: sessionMsgs})
}

// recoverFromOverflow handles context overflow by compacting and retrying.
// Returns true if recovery succeeded or was attempted. Caller must hold a.mu.
func (a *Agent) recoverFromOverflow(ctx context.Context) bool {
	a.emit(session.CompactionTrigger{
		Base:   session.BaseNow(),
		Reason: "overflow",
	})
	a.trimLastAssistantMessage()

	// Unlock for blocking compaction call
	a.mu.Unlock()
	defer a.mu.Lock()

	compacted, err := a.runCompaction(ctx)
	if err != nil {
		if !a.closed {
			a.emit(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			})
			a.emit(session.AgentEnd{Base: session.BaseNow()})
		}
		return true
	}
	if compacted {
		a.resetContextTokens()
	}

	newMessages, retryErr := a.Continue(ctx)
	if !a.closed {
		sessionMsgs := toSessionAgentMessages(newMessages)
		if retryErr != nil && !errors.Is(retryErr, context.Canceled) {
			a.emit(session.AgentEnd{Base: session.BaseNow(), Error: retryErr, Messages: sessionMsgs})
		} else {
			a.emit(session.AgentEnd{Base: session.BaseNow(), Messages: sessionMsgs})
		}
	}
	return true
}

// retryWithBackoff retries a failed turn with exponential backoff.
// Returns true if retry was attempted. Caller must hold a.mu.
func (a *Agent) retryWithBackoff(ctx context.Context, errMsg string) bool {
	a.retryAttempt++
	delayMs := a.config.GetRetryBaseDelayMs() * (1 << (a.retryAttempt - 1))

	a.emit(session.AutoRetryStart{
		Base:       session.BaseNow(),
		Attempt:    a.retryAttempt,
		MaxAttempt: a.config.GetMaxRetries(),
		DelayMs:    delayMs,
		Error:      errMsg,
	})
	a.trimLastAssistantMessage()

	// Unlock for blocking delay
	a.mu.Unlock()
	select {
	case <-ctx.Done():
		a.mu.Lock()
		if !a.closed {
			a.emit(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				Attempt:    a.retryAttempt,
				FinalError: "Retry cancelled",
			})
			a.emit(session.AgentEnd{Base: session.BaseNow()})
		}
		return true
	case <-time.After(time.Duration(delayMs) * time.Millisecond):
	}
	a.mu.Lock()

	if a.closed {
		return true
	}

	// Retry the turn (unlock for blocking call)
	a.mu.Unlock()
	newMessages, retryErr := a.Continue(ctx)
	a.mu.Lock()

	if a.closed {
		return true
	}

	sessionMsgs := toSessionAgentMessages(newMessages)
	if retryErr == nil || errors.Is(retryErr, context.Canceled) {
		a.emit(session.AutoRetryEnd{
			Base:    session.BaseNow(),
			Success: true,
			Attempt: a.retryAttempt,
		})
		a.retryAttempt = 0
		a.emit(session.AgentEnd{Base: session.BaseNow(), Messages: sessionMsgs})
		return true
	}

	// Retry failed — handle the new error (may retry again)
	handlePostAgentRunErr := retryErr
	_ = handlePostAgentRunErr // handled below after mu is re-acquired
	// Note: handlePostAgentRun acquires mu, so we must release first
	a.mu.Unlock()
	a.handlePostAgentRun(ctx, retryErr, newMessages)
	a.mu.Lock()
	return true
}

func (a *Agent) appendModelMessage(ctx context.Context, message llm.Message) error {
	a.mu.Lock()
	sess := a.sess
	a.mu.Unlock()
	if sess == nil {
		return nil
	}
	return sess.AppendModelMessage(ctx, message)
}

func (a *Agent) loadModelHistoryLocked(ctx context.Context) ([]AgentMessage, error) {
	if a.sess == nil {
		return nil, nil
	}
	messages, err := a.sess.ModelMessages(ctx)
	if err != nil {
		return nil, fmt.Errorf("load model history: %w", err)
	}
	result := make([]AgentMessage, 0, len(messages))
	for _, message := range messages {
		result = append(result, agentMessageFromLLM(message))
	}
	return result, nil
}

// trimLastAssistantMessage removes the last assistant message from agent state.
// Used during overflow recovery to remove the error message before retrying.
// Caller must hold a.mu.
func (a *Agent) trimLastAssistantMessage() {
	msgs := a.state.Messages
	if len(msgs) > 0 && msgs[len(msgs)-1].Role == "assistant" {
		a.state.Messages = msgs[:len(msgs)-1]
	}
}

// updateContextTokens updates the estimated context token count.
// Called from TokenUsage events.
// Caller must hold a.mu.
func (a *Agent) updateContextTokens(input, output int) {
	a.contextTokens += input + output
}

// needsCompaction checks if context tokens exceed the threshold.
// Returns true if compaction should be triggered.
// Caller must hold a.mu.
func (a *Agent) needsCompaction() bool {
	if a.config.Model.ContextWindow <= 0 {
		return false
	}
	// Use 80% threshold (matching Pi's default)
	threshold := int(float64(a.config.Model.ContextWindow) * 0.8)
	return a.contextTokens > threshold
}

// resetContextTokens resets the context token counter.
// Called after successful compaction.
// Caller must hold a.mu.
func (a *Agent) resetContextTokens() {
	a.contextTokens = 0
}

// runCompaction runs the compaction function if available.
// Caller must NOT hold a.mu (blocking call).
func (a *Agent) runCompaction(ctx context.Context) (bool, error) {
	a.mu.Lock()
	compactFn := a.config.CompactFunc
	closed := a.closed
	a.mu.Unlock()

	if closed {
		return false, fmt.Errorf("session is closed")
	}
	if compactFn == nil {
		return false, nil
	}
	return compactFn(ctx)
}

// SubmitTurn sends a new user turn to the active session.
func (a *Agent) SubmitTurn(ctx context.Context, input string) error {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return fmt.Errorf("session is closed")
	}
	// Cancel any active running context first
	if a.cancel != nil {
		a.cancel()
	}
	turnCtx, cancel := context.WithCancel(ctx)
	a.turnCtx = turnCtx
	a.cancel = cancel

	// Check if auto-compaction is needed before submitting
	if a.needsCompaction() && a.config.CompactFunc != nil {
		a.emit(session.CompactionTrigger{
			Base:   session.BaseNow(),
			Reason: "threshold",
		})
		a.mu.Unlock()
		compacted, err := a.config.CompactFunc(ctx)
		a.mu.Lock()
		if err != nil {
			// Log compaction error but continue with the turn
			a.emit(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			})
		} else if compacted {
			a.resetContextTokens()
		}
	}
	a.mu.Unlock()

	// Create user message
	userMsg := AgentMessage{
		Role:    "user",
		Content: input,
	}
	if _, err := a.acceptPrompts(turnCtx, []AgentMessage{userMsg}); err != nil {
		a.mu.Lock()
		a.cancel = nil
		a.turnCtx = nil
		a.mu.Unlock()
		cancel()
		return err
	}

	// Run the agent loop in a goroutine
	go func() {
		defer func() {
			a.mu.Lock()
			// Only clear if we still own the turn context.
			// A newer SubmitTurn may have replaced it.
			if a.turnCtx == turnCtx {
				a.cancel = nil
				a.turnCtx = nil
				a.overflowAttempted = false
				a.retryAttempt = 0
			}
			a.mu.Unlock()
		}()
		newMessages, err := a.Continue(turnCtx)
		a.handlePostAgentRun(turnCtx, err, newMessages)
	}()

	return nil
}

// emitEvent sends an event to the events channel without blocking.
// If the channel is full, the event is dropped to prevent deadlock.
// If the channel is closed (session shut down), the send is silently skipped.
func (a *Agent) emitEvent(ev session.AgentEvent) {
	defer func() { recover() }()
	select {
	case a.events <- ev:
	default:
		// Channel full — drop event to prevent deadlock.
	}
}

// WaitForIdle blocks until the agent is idle (no active turn).
func (a *Agent) WaitForIdle(ctx context.Context) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	// If already idle, return immediately
	if a.turnCtx == nil {
		return nil
	}

	// Wait for turn to complete
	for a.turnCtx != nil {
		a.mu.Unlock()
		select {
		case <-ctx.Done():
			a.mu.Lock()
			return ctx.Err()
		case <-time.After(10 * time.Millisecond):
			a.mu.Lock()
		}
	}

	return nil
}

// Reset clears the agent state and emits a fresh start.
func (a *Agent) Reset() {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Cancel any active turn
	if a.cancel != nil {
		a.cancel()
	}

	// Clear state
	a.state.Messages = nil
	a.state.IsStreaming = false
	a.state.ErrorMessage = ""
	a.overflowAttempted = false
	a.retryAttempt = 0
	a.contextTokens = 0

	// Clear queues
	a.steeringQueue = nil
	a.followUpQueue = nil

	// Emit fresh start
	a.emit(session.AgentStart{Base: session.BaseNow()})
}

// Subscribe registers a listener for agent events.
// Returns an unsubscribe function.
func (a *Agent) Subscribe(listener func(session.AgentEvent)) func() {
	a.mu.Lock()
	defer a.mu.Unlock()

	a.listeners = append(a.listeners, listener)

	return func() {
		a.mu.Lock()
		defer a.mu.Unlock()
		for i, l := range a.listeners {
			if fmt.Sprintf("%p", l) == fmt.Sprintf("%p", listener) {
				a.listeners = append(a.listeners[:i], a.listeners[i+1:]...)
				return
			}
		}
	}
}

// UpdateConfig updates the agent configuration.
func (a *Agent) UpdateConfig(config AgentConfig) {
	a.mu.Lock()
	defer a.mu.Unlock()

	a.config = config
	a.state.Model = config.Model
	a.state.ThinkingLevel = config.ThinkingLevel
	a.state.Tools = config.Tools
	a.state.SystemPrompt = config.SystemPrompt
}
