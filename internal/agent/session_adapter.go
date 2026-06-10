package agent

import (
	"context"
	"fmt"
	"sync"

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
			if s.closed {
				s.mu.Unlock()
				return
			}
			// Track token usage for compaction threshold (Pi: usage lives in AgentMessage)
			if msg, ok := ev.(session.AgentMessage); ok && (msg.InputTokens > 0 || msg.OutputTokens > 0) {
				s.updateContextTokens(msg.InputTokens, msg.OutputTokens)
			}
			s.mu.Unlock()
			// Send without holding lock to avoid deadlock when channel is full.
			select {
			case s.events <- ev:
			default:
				// Channel full — drop event to prevent deadlock.
			}
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
		s.emitEvent(session.CompactionTrigger{
			Base:   session.BaseNow(),
			Reason: "threshold",
		})
		s.mu.Unlock()
		compacted, err := s.config.CompactFunc(ctx)
		s.mu.Lock()
		if err != nil {
			// Log compaction error but continue with the turn
			s.emitEvent(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			})
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
				s.overflowAttempted = false
				s.retryAttempt = 0
			}
			s.mu.Unlock()
		}()
		newMessages, err := s.agent.Continue(turnCtx)
		s.handlePostAgentRun(turnCtx, err, newMessages)
	}()

	return nil
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

// emitEvent sends an event to the events channel without blocking.
// If the channel is full, the event is dropped to prevent deadlock.
// If the channel is closed (session shut down), the send is silently skipped.
func (s *SessionAdapter) emitEvent(ev session.AgentEvent) {
	defer func() { recover() }()
	select {
	case s.events <- ev:
	default:
		// Channel full — drop event to prevent deadlock.
	}
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
