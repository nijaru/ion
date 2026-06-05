package agent

import (
	"context"
	"errors"
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
	s.events <- session.QueuedInputUpdatedEvent{
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

	// Emit metadata loaded event
	s.events <- session.MetadataLoadedEvent{
		Base:      session.BaseNow(),
		SessionID: s.id,
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

	// Emit metadata loaded event
	s.events <- session.MetadataLoadedEvent{
		Base:      session.BaseNow(),
		SessionID: s.id,
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
			s.cancel = nil
			s.turnCtx = nil
			s.overflowAttempted = false
			s.mu.Unlock()
		}()
		_, err := s.agent.Continue(turnCtx)
		s.mu.Lock()
		if !s.closed {
			if err != nil && !errors.Is(err, context.Canceled) {
				// Check if this is a context overflow error
				if IsContextOverflow(err.Error()) && !s.overflowAttempted {
					s.overflowAttempted = true
					// Emit compaction triggered event
					s.events <- session.CompactionTriggeredEvent{
						Base:  session.BaseNow(),
						Reason: "overflow",
					}
					// Remove last assistant message (the error) from agent state
					s.trimLastAssistantMessage()
					s.mu.Unlock()
					// TODO: Run actual compaction here
					// For now, just retry the turn
					_, retryErr := s.agent.Continue(turnCtx)
					s.mu.Lock()
					if !s.closed {
						if retryErr != nil && !errors.Is(retryErr, context.Canceled) {
							s.events <- session.ErrorEvent{
								Base:  session.BaseNow(),
								Err:   retryErr,
								Fatal: true,
							}
						}
						s.events <- session.TurnFinishedEvent{Base: session.BaseNow()}
					}
				} else {
					s.events <- session.ErrorEvent{
						Base:  session.BaseNow(),
						Err:   err,
						Fatal: true,
					}
					s.events <- session.TurnFinishedEvent{Base: session.BaseNow()}
				}
			} else {
				s.events <- session.TurnFinishedEvent{Base: session.BaseNow()}
			}
		}
		s.mu.Unlock()
	}()

	return nil
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
