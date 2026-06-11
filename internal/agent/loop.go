// Package agent provides the core agent loop primitive for Ion.
//
// The agent loop is a pure turn-sequencing function. It streams assistant
// responses, executes tool calls, and repeats until the model stops or a
// callback says to stop. It does not manage persistence, recovery, or
// session lifecycle — those are separate concerns composed around the loop.
package agent

import (
	"context"
	"errors"
	"fmt"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// AgentLoop is the pure agent turn-sequencing loop.
//
// It owns:
//   - Turn sequencing (stream → tool calls → check stop → repeat)
//   - Event emission (agent_start/end, turn_start/end, message_start/end, deltas, tool events)
//   - Steering and follow-up message injection
//
// It does NOT own:
//   - Persistence (writeModelMessage is a callback)
//   - Recovery (overflow/retry is a wrapper)
//   - Session lifecycle (Open/Resume/Close are on the wrapper)
//   - Queue management (GetSteeringMessages/GetFollowUpMessages are callbacks)
type AgentLoop struct {
	config AgentConfig
	state  AgentState
	emit   func(session.AgentEvent)
}

// NewAgentLoop creates a new pure agent loop.
func NewAgentLoop(config AgentConfig, state AgentState, emit func(session.AgentEvent)) *AgentLoop {
	return &AgentLoop{
		config: config,
		state:  state,
		emit:   emit,
	}
}

// Run starts the agent loop with prompt messages.
//
// It:
//  1. Appends prompts to the message history
//  2. Emits message_start/end for each prompt
//  3. Delegates to runLoop for turn sequencing
//
// Returns the new messages added during the run.
// Emits: agent_start, turn_start, message_start/end (per prompt), then runLoop events.
func (l *AgentLoop) Run(ctx context.Context, prompts []AgentMessage) ([]AgentMessage, error) {
	l.emit(session.AgentStart{Base: session.BaseNow()})

	// Append prompts to state
	l.state.Messages = append(l.state.Messages, prompts...)

	// Emit message events for prompts and persist them
	var newMessages []AgentMessage
	for _, prompt := range prompts {
		newMessages = append(newMessages, prompt)
		if prompt.Role == "user" {
			l.emit(session.UserMessage{
				Base:    session.BaseNow(),
				Message: prompt.Content,
			})
		}
		if err := l.writeModelMessage(ctx, agentMessageToLLM(prompt)); err != nil {
			return newMessages, fmt.Errorf("write prompt message: %w", err)
		}
	}

	// Run the turn loop
	loopMessages, err := l.runLoop(ctx)
	newMessages = append(newMessages, loopMessages...)

	return newMessages, err
}

// Continue continues the agent loop without adding new messages.
//
// Used for retries — context already has user message or tool results.
// Returns the new messages added during the run.
// Emits: agent_start, then runLoop events.
func (l *AgentLoop) Continue(ctx context.Context) ([]AgentMessage, error) {
	l.emit(session.AgentStart{Base: session.BaseNow()})

	if len(l.state.Messages) == 0 {
		err := fmt.Errorf("cannot continue: no messages in context")
		l.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
		return nil, err
	}

	lastMsg := l.state.Messages[len(l.state.Messages)-1]
	if lastMsg.Role == "assistant" {
		err := fmt.Errorf("cannot continue from message role: assistant")
		l.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
		return nil, err
	}

	return l.runLoop(ctx)
}

// runLoop is the pure turn-sequencing loop.
//
// It:
//  1. Checks for steering messages
//  2. Streams assistant response
//  3. Executes tool calls if any
//  4. Checks shouldStopAfterTurn
//  5. Checks for follow-up messages
//  6. Repeats or exits
//
// Returns the new messages added during the loop.
func (l *AgentLoop) runLoop(ctx context.Context) ([]AgentMessage, error) {
	var newMessages []AgentMessage
	var pendingMessages []AgentMessage

	for {
		hasMoreToolCalls := true

		// Check for steering messages at start of iteration
		if len(pendingMessages) == 0 {
			pendingMessages = l.getSteeringMessages()
		}

		// Inner loop: process tool calls and steering messages
		for hasMoreToolCalls || len(pendingMessages) > 0 {
			// Check for context cancellation
			if ctx.Err() != nil {
				l.emit(session.TurnEnd{Base: session.BaseNow()})
				return newMessages, ctx.Err()
			}

			l.emit(session.TurnStart{Base: session.BaseNow()})

			// Inject pending messages
			if len(pendingMessages) > 0 {
				l.state.Messages = append(l.state.Messages, pendingMessages...)
				newMessages = append(newMessages, pendingMessages...)
				for _, msg := range pendingMessages {
					if msg.Role == "user" {
						l.emit(session.UserMessage{
							Base:    session.BaseNow(),
							Message: msg.Content,
						})
					}
					if err := l.writeModelMessage(ctx, agentMessageToLLM(msg)); err != nil {
						l.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
						return newMessages, fmt.Errorf("write pending message: %w", err)
					}
				}
				pendingMessages = nil
			}

			// Stream assistant response
			message, llmMessage, err := l.streamAssistantResponse(ctx)
			if err != nil {
				if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
					l.emit(session.TurnEnd{Base: session.BaseNow()})
				} else {
					l.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
				}
				return newMessages, fmt.Errorf("stream assistant response: %w", err)
			}

			// Add assistant message to state
			l.state.Messages = append(l.state.Messages, message)
			newMessages = append(newMessages, message)

			// Emit complete assistant message with usage
			l.emit(session.AgentMessage{
				Base:         session.BaseNow(),
				Message:      message.Content,
				Reasoning:    message.Reasoning,
				InputTokens:  message.InputTokens,
				OutputTokens: message.OutputTokens,
				TotalTokens:  message.TotalTokens,
				Cost:         message.Cost,
			})
			if err := l.writeModelMessage(ctx, llmMessage); err != nil {
				l.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
				return newMessages, fmt.Errorf("write assistant message: %w", err)
			}

			// Check for error/abort
			if message.IsError {
				l.emit(session.TurnEnd{Base: session.BaseNow()})
				return newMessages, nil
			}

			// Execute tool calls if any
			toolCalls := message.Calls
			hasMoreToolCalls = false
			var toolResults []AgentMessage

			if len(toolCalls) > 0 {
				var terminate bool
				var llmToolResults []llm.Message
				toolResults, llmToolResults, terminate, err = l.executeToolCalls(
					ctx, message, llmMessage, toolCalls,
				)
				if err != nil {
					l.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
					return newMessages, fmt.Errorf("execute tool calls: %w", err)
				}

				hasMoreToolCalls = !terminate

				// Emit message events for tool results
				for _, result := range toolResults {
					l.emit(session.MessageStart{Base: session.BaseNow(), Message: session.AgentMessage{
						Message: result.Content,
					}})
					l.emit(session.MessageEnd{Base: session.BaseNow(), Message: session.AgentMessage{
						Message: result.Content,
					}})
				}

				// Add tool results to context and persist
				l.state.Messages = append(l.state.Messages, toolResults...)
				newMessages = append(newMessages, toolResults...)
				for _, result := range llmToolResults {
					if err := l.writeModelMessage(ctx, result); err != nil {
						l.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
						return newMessages, fmt.Errorf("write tool result: %w", err)
					}
				}
			}

			// Emit turn_end
			l.emit(session.TurnEnd{
				Base: session.BaseNow(),
				Message: session.AgentMessage{
					Message:      message.Content,
					Reasoning:    message.Reasoning,
					InputTokens:  message.InputTokens,
					OutputTokens: message.OutputTokens,
					TotalTokens:  message.TotalTokens,
					Cost:         message.Cost,
				},
				ToolResults: toSessionAgentMessages(toolResults),
			})

			// Prepare next turn (may update context/model/thinking level)
			turnContext := ShouldStopAfterTurnContext{
				Message:     llmMessage,
				ToolResults: agentMessagesToLLM(toolResults),
				Context:     l.buildContext(),
				NewMessages: cloneAgentMessages(newMessages),
			}
			if l.config.PrepareNextTurn != nil {
				l.applyTurnUpdate(l.config.PrepareNextTurn(turnContext))
				turnContext.Context = l.buildContext()
			}

			// Check if we should stop
			if l.config.ShouldStopAfterTurn != nil {
				if l.config.ShouldStopAfterTurn(turnContext) {
					return newMessages, nil
				}
			}

			// Get steering messages for next iteration
			pendingMessages = l.getSteeringMessages()
		}

		// Agent would stop here. Check for follow-up messages.
		followUpMessages := l.getFollowUpMessages()
		if len(followUpMessages) > 0 {
			pendingMessages = followUpMessages
			continue
		}

		// No more messages, exit
		return newMessages, nil
	}
}

// getSteeringMessages returns steering messages from the config hook.
func (l *AgentLoop) getSteeringMessages() []AgentMessage {
	if l.config.GetSteeringMessages != nil {
		return l.config.GetSteeringMessages()
	}
	return nil
}

// getFollowUpMessages returns follow-up messages from the config hook.
func (l *AgentLoop) getFollowUpMessages() []AgentMessage {
	if l.config.GetFollowUpMessages != nil {
		return l.config.GetFollowUpMessages()
	}
	return nil
}

// buildContext builds the current AgentContext from the loop state.
func (l *AgentLoop) buildContext() AgentContext {
	return AgentContext{
		Messages:      l.state.Messages,
		SystemPrompt:  l.state.SystemPrompt,
		Tools:         l.state.Tools,
		Model:         l.state.Model,
		ThinkingLevel: l.state.ThinkingLevel,
	}
}

// applyTurnUpdate applies a turn update to the loop state.
func (l *AgentLoop) applyTurnUpdate(update *AgentLoopTurnUpdate) {
	if update == nil {
		return
	}
	if update.Context != nil {
		l.state.Messages = cloneAgentMessages(update.Context.Messages)
		l.state.SystemPrompt = update.Context.SystemPrompt
		l.state.Tools = append([]AgentTool(nil), update.Context.Tools...)
		l.state.Model = update.Context.Model
		l.state.ThinkingLevel = update.Context.ThinkingLevel
	}
	if update.Model != nil {
		l.state.Model = *update.Model
	}
	if update.ThinkingLevel != nil {
		l.state.ThinkingLevel = *update.ThinkingLevel
	}
}

// writeModelMessage persists a message through the config callback.
func (l *AgentLoop) writeModelMessage(ctx context.Context, message llm.Message) error {
	if l.config.OnModelMessage == nil {
		return nil
	}
	if isEmptyModelMessage(message) {
		return nil
	}
	return l.config.OnModelMessage(ctx, message)
}

// Messages returns the current message history.
func (l *AgentLoop) Messages() []AgentMessage {
	return l.state.Messages
}

// State returns the current loop state.
func (l *AgentLoop) State() AgentState {
	return l.state
}
