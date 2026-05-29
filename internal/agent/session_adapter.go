package agent

import (
	"context"
	"fmt"
	"sync"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// SessionAdapter wraps an Agent to implement session.AgentSession,
// session.SteeringSession, and session.QueuedInputSession interfaces.
type SessionAdapter struct {
	agent  *Agent
	store  storage.Store
	sess   storage.Session
	config *SessionAdapterConfig

	mu        sync.Mutex
	id        string
	events    chan session.Event
	closed    bool
	closeOnce sync.Once
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
}

// NewSessionAdapter creates a new session adapter.
func NewSessionAdapter(config *SessionAdapterConfig) *SessionAdapter {
	if config.ID == "" {
		config.ID = "default"
	}

	agentConfig := AgentLoopConfig{
		Model:         config.Model,
		ThinkingLevel: config.ThinkingLevel,
		MaxTokens:     config.MaxTokens,
		Temperature:   config.Temperature,
		StreamFn:      config.StreamFn,
		ToolExecutor:  config.ToolExecutor,
	}

	agent := New(agentConfig)
	if config.SystemPrompt != "" {
		agent.SetSystemPrompt(config.SystemPrompt)
	}
	if len(config.Tools) > 0 {
		agent.SetTools(config.Tools)
	}

	return &SessionAdapter{
		agent:  agent,
		config: config,
		id:     config.ID,
		events: make(chan session.Event, 100),
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
	s.events <- session.MetadataLoaded{
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

	// TODO: Load session state from store

	// Emit metadata loaded event
	s.events <- session.MetadataLoaded{
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
	s.mu.Unlock()

	// Create user message
	userMsg := AgentMessage{
		Role:    "user",
		Content: input,
	}

	// Run the agent loop in a goroutine
	go func() {
		newMessages, err := s.agent.Run(ctx, []AgentMessage{userMsg})
		if err != nil {
			s.events <- session.Error{
				Base: session.BaseNow(),
				Err:  err,
				Fatal: true,
			}
			return
		}

		// Emit events for new messages
		for _, msg := range newMessages {
			switch msg.Role {
			case "user":
				s.events <- session.UserMessage{
					Base:    session.BaseNow(),
					Message: msg.Content,
				}
			case "assistant":
				s.events <- session.AgentMessage{
					Base:      session.BaseNow(),
					Message:   msg.Content,
					Reasoning: msg.Reasoning,
				}
			case "tool":
				s.events <- session.ToolResult{
					Base:      session.BaseNow(),
					ToolUseID: msg.ToolID,
					Result:    msg.Content,
				}
			}
		}

		// Emit turn complete event
		s.events <- session.TurnFinished{
			Base: session.BaseNow(),
		}
	}()

	return nil
}

// CancelTurn interrupts an in-flight turn if the backend supports it.
func (s *SessionAdapter) CancelTurn(ctx context.Context) error {
	// TODO: Implement cancellation
	return nil
}

// Close terminates the session and cleans up resources.
func (s *SessionAdapter) Close() error {
	s.closeOnce.Do(func() {
		s.mu.Lock()
		s.closed = true
		s.mu.Unlock()
		close(s.events)
	})
	return nil
}

// Events returns a read-only channel of typed events emitted by the session.
func (s *SessionAdapter) Events() <-chan session.Event {
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
func (s *SessionAdapter) SteerTurn(ctx context.Context, text string) (session.SteeringResult, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return session.SteeringResult{}, fmt.Errorf("session is closed")
	}
	s.mu.Unlock()

	// TODO: Implement steering
	return session.SteeringResult{
		Outcome: session.SteeringQueued,
		Notice:  "Steering input queued",
	}, nil
}

// FollowUpTurn sends follow-up input after the agent would stop.
func (s *SessionAdapter) FollowUpTurn(ctx context.Context, text string) (session.QueuedInputResult, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return session.QueuedInputResult{}, fmt.Errorf("session is closed")
	}
	s.mu.Unlock()

	// TODO: Implement follow-up
	return session.QueuedInputResult{
		Outcome: session.QueuedInputQueued,
		Notice:  "Follow-up input queued",
	}, nil
}

// ClearQueuedInput clears queued input and returns the snapshot.
func (s *SessionAdapter) ClearQueuedInput(ctx context.Context) (session.QueuedInputSnapshot, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return session.QueuedInputSnapshot{}, fmt.Errorf("session is closed")
	}
	s.mu.Unlock()

	// TODO: Implement clear queued input
	return session.QueuedInputSnapshot{}, nil
}

// SetStore sets the storage store.
func (s *SessionAdapter) SetStore(store storage.Store) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.store = store
}

// SetSession sets the storage session.
func (s *SessionAdapter) SetSession(sess storage.Session) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.sess = sess
}
