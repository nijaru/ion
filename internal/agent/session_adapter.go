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

// SessionAdapter wraps an Agent to implement session.AgentSession,
// session.SteeringSession, and session.QueuedInputSession interfaces.
type SessionAdapter struct {
	agent  *Agent
	store  session.SessionStore
	sess   session.SessionHandle
	config *SessionAdapterConfig

	mu            sync.Mutex
	id            string
	events        chan session.AgentEvent
	closed        bool
	closeOnce     sync.Once
	steeringQueue []string
	followUpQueue []string
	turnCtx       context.Context
	cancel        context.CancelFunc

	// overflowAttempted tracks if we've already tried overflow recovery.
	// Only one retry attempt is allowed per turn (matching Pi's
	// _overflowRecoveryAttempted guard).
	overflowAttempted bool

	// retryAttempt tracks the current retry attempt for transient errors.
	// Reset to 0 after successful completion or max retries exceeded.
	retryAttempt int

	// contextTokens tracks the estimated context token count.
	// Updated from TokenUsage events.
	contextTokens int
}

// SessionAdapterConfig holds configuration for the session adapter.
type SessionAdapterConfig struct {
	// ID is the session identifier.
	ID string
	// SystemPrompt is the system prompt for the agent.
	SystemPrompt string
	// Tools are the available tools.
	Tools []AgentTool
	// Model is the initial model.
	Model llm.Model
	// ThinkingLevel is the initial thinking level.
	ThinkingLevel ThinkingLevel
	// MaxTokens is the maximum tokens for responses.
	MaxTokens int
	// Temperature is the temperature for responses.
	Temperature float64
	// StreamFn is the function to stream LLM responses.
	StreamFn StreamFn
	// ToolExecutor executes tool calls.
	ToolExecutor ToolExecutor
	// QueueMode controls how many queued inputs are consumed at a loop boundary.
	QueueMode QueueMode

	// MaxRetries is the max number of retry attempts for transient errors.
	// Default: 3
	MaxRetries int
	// RetryBaseDelayMs is the base delay in ms for exponential backoff.
	// Default: 1000
	RetryBaseDelayMs int

	// CompactFunc is the function to call for compaction.
	// If nil, compaction is skipped.
	CompactFunc func(ctx context.Context) (bool, error)
}

const (
	defaultMaxRetries       = 3
	defaultRetryBaseDelayMs = 1000
)

// GetMaxRetries returns the max retry attempts (default 3).
func (c *SessionAdapterConfig) GetMaxRetries() int {
	if c == nil || c.MaxRetries <= 0 {
		return defaultMaxRetries
	}
	if c.MaxRetries > 10 {
		return 10
	}
	return c.MaxRetries
}

// GetRetryBaseDelayMs returns the base delay in ms for exponential backoff (default 1000).
func (c *SessionAdapterConfig) GetRetryBaseDelayMs() int {
	if c == nil || c.RetryBaseDelayMs <= 0 {
		return defaultRetryBaseDelayMs
	}
	if c.RetryBaseDelayMs > 60000 {
		return 60000
	}
	return c.RetryBaseDelayMs
}

// NewSessionAdapter creates a new session adapter.
func NewSessionAdapter(config *SessionAdapterConfig) *SessionAdapter {
	if config.ID == "" {
		config.ID = "default"
	}

	s := &SessionAdapter{
		config: config,
		id:     config.ID,
		events: make(chan session.AgentEvent, 100),
	}

	queueMode := config.QueueMode
	if queueMode == "" {
		queueMode = QueueModeOneAtATime
	}

	agentConfig := AgentLoopConfig{
		Model:         config.Model,
		ThinkingLevel: config.ThinkingLevel,
		QueueMode:     queueMode,
		MaxTokens:     config.MaxTokens,
		Temperature:   config.Temperature,
		StreamFn:      config.StreamFn,
		ToolExecutor:  config.ToolExecutor,
		OnEvent: func(ev session.AgentEvent) {
			s.mu.Lock()
			defer s.mu.Unlock()
			if s.closed {
				return
			}
			// Track token usage for compaction threshold (Pi: usage lives in AgentMessage)
			if msg, ok := ev.(session.AgentMessage); ok && (msg.InputTokens > 0 || msg.OutputTokens > 0) {
				s.updateContextTokens(msg.InputTokens, msg.OutputTokens)
			}
			s.events <- ev
		},
		OnModelMessage: s.appendModelMessage,
		GetSteeringMessages: func() []AgentMessage {
			s.mu.Lock()
			defer s.mu.Unlock()
			if len(s.steeringQueue) == 0 {
				return nil
			}
			msgs := drainQueuedMessagesLocked(&s.steeringQueue, queueMode)
			s.emitQueueUpdatedLocked()
			return msgs
		},
		GetFollowUpMessages: func() []AgentMessage {
			s.mu.Lock()
			defer s.mu.Unlock()
			if len(s.followUpQueue) == 0 {
				return nil
			}
			msgs := drainQueuedMessagesLocked(&s.followUpQueue, queueMode)
			s.emitQueueUpdatedLocked()
			return msgs
		},
	}

	agent := New(agentConfig)
	if config.SystemPrompt != "" {
		agent.SetSystemPrompt(config.SystemPrompt)
	}
	if len(config.Tools) > 0 {
		agent.SetTools(config.Tools)
	}

	s.agent = agent
	return s
}

func drainQueuedMessagesLocked(queue *[]string, mode QueueMode) []AgentMessage {
	if len(*queue) == 0 {
		return nil
	}
	count := 1
	if mode == QueueModeAll {
		count = len(*queue)
	}
	msgs := make([]AgentMessage, count)
	for i, text := range (*queue)[:count] {
		msgs[i] = AgentMessage{Role: "user", Content: text}
	}
	*queue = (*queue)[count:]
	return msgs
}

func (s *SessionAdapter) emitQueueUpdatedLocked() {
	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), s.steeringQueue...),
		FollowUp: append([]string(nil), s.followUpQueue...),
	}
	s.events <- session.QueuedInputUpdate{
		Base:     session.BaseNow(),
		Snapshot: snapshot,
	}
}

// Open initializes or creates a new session.
func (s *SessionAdapter) Open(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return fmt.Errorf("session is closed")
	}

	return nil
}

// Resume loads an existing session.
func (s *SessionAdapter) Resume(ctx context.Context, sessionID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return fmt.Errorf("session is closed")
	}

	s.id = sessionID

	if history, err := s.loadModelHistoryLocked(ctx); err != nil {
		return err
	} else if history != nil {
		s.agent.SetMessages(history)
	}

	return nil
}

// SubmitTurn sends a new user turn to the active session.
func (s *SessionAdapter) SubmitTurn(ctx context.Context, input string) error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return fmt.Errorf("session is closed")
	}
	// Cancel any active running context first
	if s.cancel != nil {
		s.cancel()
	}
	turnCtx, cancel := context.WithCancel(ctx)
	s.turnCtx = turnCtx
	s.cancel = cancel

	// Check if auto-compaction is needed before submitting
	if s.needsCompaction() && s.config.CompactFunc != nil {
		s.events <- session.CompactionTrigger{
			Base:   session.BaseNow(),
			Reason: "threshold",
		}
		s.mu.Unlock()
		compacted, err := s.config.CompactFunc(ctx)
		s.mu.Lock()
		if err != nil {
			// Log compaction error but continue with the turn
			s.events <- session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			}
		} else if compacted {
			s.resetContextTokens()
		}
	}
	s.mu.Unlock()

	// Create user message
	userMsg := AgentMessage{
		Role:    "user",
		Content: input,
	}
	if _, err := s.agent.acceptPrompts(turnCtx, []AgentMessage{userMsg}); err != nil {
		s.mu.Lock()
		s.cancel = nil
		s.turnCtx = nil
		s.mu.Unlock()
		cancel()
		return err
	}

	// Run the agent loop in a goroutine
	go func() {
		defer func() {
			s.mu.Lock()
			// Only clear if we still own the turn context.
			// A newer SubmitTurn may have replaced it.
			if s.turnCtx == turnCtx {
				s.cancel = nil
				s.turnCtx = nil
			}
			s.overflowAttempted = false
			s.retryAttempt = 0
			s.mu.Unlock()
		}()
		_, err := s.agent.Continue(turnCtx)
		s.handlePostAgentRun(turnCtx, err)
	}()

	return nil
}

// handlePostAgentRun handles post-agent-run logic including overflow
// recovery and auto-retry with exponential backoff.
// Emits AgentEnd when all recovery is exhausted (Pi parity).
func (s *SessionAdapter) handlePostAgentRun(ctx context.Context, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return
	}

	// Success or cancellation — agent already emitted TurnEnd
	if err == nil || errors.Is(err, context.Canceled) {
		s.retryAttempt = 0
		s.events <- session.AgentEnd{Base: session.BaseNow()}
		return
	}

	errMsg := err.Error()

	// Overflow recovery: compact and retry once
	if IsContextOverflow(errMsg) && !s.overflowAttempted {
		s.overflowAttempted = true
		if s.recoverFromOverflow(ctx) {
			return
		}
	}

	// Transient error retry with exponential backoff
	if IsRetryableError(errMsg) && s.retryAttempt < s.config.GetMaxRetries() {
		if s.retryWithBackoff(ctx, errMsg) {
			return
		}
	}

	// Non-retryable error — agent already emitted TurnEnd{Error}
	s.events <- session.AgentEnd{Base: session.BaseNow(), Error: err}
}

// recoverFromOverflow handles context overflow by compacting and retrying.
// Returns true if recovery succeeded or was attempted. Caller must hold s.mu.
func (s *SessionAdapter) recoverFromOverflow(ctx context.Context) bool {
	s.events <- session.CompactionTrigger{
		Base:   session.BaseNow(),
		Reason: "overflow",
	}
	s.trimLastAssistantMessage()

	// Unlock for blocking compaction call
	s.mu.Unlock()
	defer s.mu.Lock()

	compacted, err := s.runCompaction(ctx)
	if err != nil {
		if !s.closed {
			s.events <- session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			}
			s.events <- session.AgentEnd{Base: session.BaseNow()}
		}
		return true
	}
	if compacted {
		s.resetContextTokens()
	}

	_, retryErr := s.agent.Continue(ctx)
	if !s.closed {
		if retryErr != nil && !errors.Is(retryErr, context.Canceled) {
			s.events <- session.AgentEnd{Base: session.BaseNow(), Error: retryErr}
		} else {
			s.events <- session.AgentEnd{Base: session.BaseNow()}
		}
	}
	return true
}

// retryWithBackoff retries a failed turn with exponential backoff.
// Returns true if retry was attempted. Caller must hold s.mu.
func (s *SessionAdapter) retryWithBackoff(ctx context.Context, errMsg string) bool {
	s.retryAttempt++
	delayMs := s.config.GetRetryBaseDelayMs() * (1 << (s.retryAttempt - 1))

	s.events <- session.AutoRetryStart{
		Base:       session.BaseNow(),
		Attempt:    s.retryAttempt,
		MaxAttempt: s.config.GetMaxRetries(),
		DelayMs:    delayMs,
		Error:      errMsg,
	}
	s.trimLastAssistantMessage()

	// Unlock for blocking delay
	s.mu.Unlock()
	select {
	case <-ctx.Done():
		s.mu.Lock()
		if !s.closed {
			s.events <- session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				Attempt:    s.retryAttempt,
				FinalError: "Retry cancelled",
			}
			s.events <- session.AgentEnd{Base: session.BaseNow()}
		}
		return true
	case <-time.After(time.Duration(delayMs) * time.Millisecond):
	}
	s.mu.Lock()

	if s.closed {
		return true
	}

	// Retry the turn (unlock for blocking call)
	s.mu.Unlock()
	_, retryErr := s.agent.Continue(ctx)
	s.mu.Lock()

	if s.closed {
		return true
	}

	if retryErr == nil || errors.Is(retryErr, context.Canceled) {
		s.events <- session.AutoRetryEnd{
			Base:    session.BaseNow(),
			Success: true,
			Attempt: s.retryAttempt,
		}
		s.retryAttempt = 0
		s.events <- session.AgentEnd{Base: session.BaseNow()}
		return true
	}

	// Retry failed — handle the new error (may retry again)
	handlePostAgentRunErr := retryErr
	_ = handlePostAgentRunErr // handled below after mu is re-acquired
	// Note: handlePostAgentRun acquires mu, so we must release first
	s.mu.Unlock()
	s.handlePostAgentRun(ctx, retryErr)
	s.mu.Lock()
	return true
}


func (s *SessionAdapter) appendModelMessage(ctx context.Context, message llm.Message) error {
	s.mu.Lock()
	sess := s.sess
	s.mu.Unlock()
	if sess == nil {
		return nil
	}
	return sess.AppendModelMessage(ctx, message)
}

func (s *SessionAdapter) loadModelHistoryLocked(ctx context.Context) ([]AgentMessage, error) {
	if s.sess == nil {
		return nil, nil
	}
	messages, err := s.sess.ModelMessages(ctx)
	if err != nil {
		return nil, fmt.Errorf("load model history: %w", err)
	}
	result := make([]AgentMessage, 0, len(messages))
	for _, message := range messages {
		result = append(result, agentMessageFromLLM(message))
	}
	return result, nil
}

// CancelTurn interrupts an in-flight turn if the backend supports it.
func (s *SessionAdapter) CancelTurn(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return fmt.Errorf("session is closed")
	}

	if s.cancel != nil {
		s.cancel()
	}
	return nil
}

// Close terminates the session and cleans up resources.
func (s *SessionAdapter) Close() error {
	s.closeOnce.Do(func() {
		s.mu.Lock()
		s.closed = true
		if s.cancel != nil {
			s.cancel()
		}
		s.mu.Unlock()
		close(s.events)
	})
	return nil
}

// Events returns a read-only channel of typed events emitted by the session.
func (s *SessionAdapter) Events() <-chan session.AgentEvent {
	return s.events
}

// ID returns the session identifier.
func (s *SessionAdapter) ID() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.id
}

// Meta returns session metadata.
func (s *SessionAdapter) Meta() map[string]string {
	s.mu.Lock()
	defer s.mu.Unlock()

	return map[string]string{
		"backend": "agent",
		"model":   s.config.Model.ID,
	}
}

// SteerTurn sends steering input during an active turn.
func (s *SessionAdapter) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return session.SteeringResult{}, fmt.Errorf("session is closed")
	}
	s.steeringQueue = append(s.steeringQueue, text)
	s.emitQueueUpdatedLocked()
	s.mu.Unlock()

	return session.SteeringResult{
		Outcome: session.SteeringAccepted,
		Notice:  "Steering input accepted",
	}, nil
}

// FollowUpTurn sends follow-up input after the agent would stop.
func (s *SessionAdapter) FollowUpTurn(
	ctx context.Context,
	text string,
) (session.QueuedInputResult, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return session.QueuedInputResult{}, fmt.Errorf("session is closed")
	}
	s.followUpQueue = append(s.followUpQueue, text)
	s.emitQueueUpdatedLocked()
	s.mu.Unlock()

	return session.QueuedInputResult{
		Outcome: session.QueuedInputAccepted,
		Notice:  "Follow-up input accepted",
	}, nil
}

// ClearQueuedInput clears queued input and returns the snapshot.
func (s *SessionAdapter) ClearQueuedInput(
	ctx context.Context,
) (session.QueuedInputSnapshot, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return session.QueuedInputSnapshot{}, fmt.Errorf("session is closed")
	}

	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), s.steeringQueue...),
		FollowUp: append([]string(nil), s.followUpQueue...),
	}

	s.steeringQueue = nil
	s.followUpQueue = nil
	s.emitQueueUpdatedLocked()

	return snapshot, nil
}

// SetStore sets the storage store.
func (s *SessionAdapter) SetStore(store session.SessionStore) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.store = store
}

// SetSession sets the storage session.
func (s *SessionAdapter) SetSession(sess session.SessionHandle) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.sess = sess
}

// trimLastAssistantMessage removes the last assistant message from agent state.
// Used during overflow recovery to remove the error message before retrying.
// Caller must hold s.mu.
func (s *SessionAdapter) trimLastAssistantMessage() {
	msgs := s.agent.state.Messages
	if len(msgs) > 0 && msgs[len(msgs)-1].Role == "assistant" {
		s.agent.state.Messages = msgs[:len(msgs)-1]
	}
}

// updateContextTokens updates the estimated context token count.
// Called from TokenUsage events.
// Caller must hold s.mu.
func (s *SessionAdapter) updateContextTokens(input, output int) {
	s.contextTokens += input + output
}

// needsCompaction checks if context tokens exceed the threshold.
// Returns true if compaction should be triggered.
// Caller must hold s.mu.
func (s *SessionAdapter) needsCompaction() bool {
	if s.config == nil {
		return false
	}
	contextWindow := s.config.Model.ContextWindow
	if contextWindow <= 0 {
		return false
	}
	// Use 80% threshold (matching Pi's default)
	threshold := int(float64(contextWindow) * 0.8)
	return s.contextTokens > threshold
}

// resetContextTokens resets the context token counter.
// Called after successful compaction.
// Caller must hold s.mu.
func (s *SessionAdapter) resetContextTokens() {
	s.contextTokens = 0
}

// runCompaction runs the compaction function if available.
// Caller must NOT hold s.mu (blocking call).
func (s *SessionAdapter) runCompaction(ctx context.Context) (bool, error) {
	s.mu.Lock()
	compactFn := s.config.CompactFunc
	closed := s.closed
	s.mu.Unlock()

	if closed {
		return false, fmt.Errorf("session is closed")
	}
	if compactFn == nil {
		return false, nil
	}
	return compactFn(ctx)
}
