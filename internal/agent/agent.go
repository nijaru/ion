package agent

import (
	"context"
	"fmt"
	"sync"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Agent is the core agent loop primitive. It manages the lifecycle of an
// agent session: submit → stream → tool calls → results → done.
type Agent struct {
	config AgentConfig
	state  AgentState
	mu     sync.RWMutex
}

// New creates a new Agent with the given configuration.
func New(config AgentConfig) *Agent {
	return &Agent{
		config: config,
		state: AgentState{
			Model:         config.Model,
			ThinkingLevel: config.ThinkingLevel,
			Tools:         []AgentTool{},
		},
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
