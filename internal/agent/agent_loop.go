package agent

import (
	"context"
	"errors"
	"fmt"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// runLoop is the main agent loop logic.
func (a *Agent) runLoop(ctx context.Context, newMessages *[]AgentMessage) error {
	var pendingMessages []AgentMessage

	// Outer loop: continues when queued follow-up messages arrive
	for {
		hasMoreToolCalls := true

		// Check for steering messages at start
		if len(pendingMessages) == 0 {
			pendingMessages = a.getSteeringMessages()
		}

		// Inner loop: process tool calls and steering messages
		for hasMoreToolCalls || len(pendingMessages) > 0 {
			// Check for context cancellation
			if ctx.Err() != nil {
				// Cancellation is normal control flow, not an error.
				a.emit(session.TurnEnd{Base: session.BaseNow()})
				return ctx.Err()
			}

			a.emit(session.TurnStart{Base: session.BaseNow()})

			// Process pending messages (inject before next assistant response)
			if len(pendingMessages) > 0 {
				a.mu.Lock()
				a.state.Messages = append(a.state.Messages, pendingMessages...)
				a.mu.Unlock()
				*newMessages = append(*newMessages, pendingMessages...)
				for _, message := range pendingMessages {
					a.emitInputMessage(message)
					if err := a.writeModelMessage(ctx, agentMessageToLLM(message)); err != nil {
						return err
					}
				}
				pendingMessages = nil
			}

			// Stream assistant response
			message, llmMessage, err := a.streamAssistantResponse(ctx)
			if err != nil {
				// Cancellation is normal control flow, not an error.
				if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
					a.emit(session.TurnEnd{Base: session.BaseNow()})
				} else {
					a.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
				}
				return fmt.Errorf("stream assistant response: %w", err)
			}

			a.mu.Lock()
			a.state.Messages = append(a.state.Messages, message)
			a.mu.Unlock()
			*newMessages = append(*newMessages, message)

			// Emit complete assistant message event with usage (Pi: usage in message_end)
			a.emit(session.AgentMessage{
				Base:         session.BaseNow(),
				Message:      message.Content,
				Reasoning:    message.Reasoning,
				InputTokens:  message.InputTokens,
				OutputTokens: message.OutputTokens,
				TotalTokens:  message.TotalTokens,
				Cost:         message.Cost,
			})
			if err := a.writeModelMessage(ctx, llmMessage); err != nil {
				return err
			}

			// Check for error/abort
			if message.IsError {
				a.emit(session.TurnEnd{Base: session.BaseNow()})
				return nil
			}

			// Check for tool calls
			toolCalls := message.Calls
			hasMoreToolCalls = false
			var toolResults []AgentMessage
			var llmToolResults []llm.Message

			if len(toolCalls) > 0 {
				var terminate bool
				toolResults, llmToolResults, terminate, err = a.executeToolCalls(
					ctx,
					message,
					llmMessage,
					toolCalls,
				)
				if err != nil {
					a.emit(session.TurnEnd{Base: session.BaseNow(), Error: err})
					return fmt.Errorf("execute tool calls: %w", err)
				}

				hasMoreToolCalls = !terminate

				// Emit MessageStart/MessageEnd for tool results (Pi: message lifecycle)
				for _, result := range toolResults {
					a.emit(session.MessageStart{Base: session.BaseNow(), Message: session.AgentMessage{
						Message: result.Content,
					}})
					a.emit(session.MessageEnd{Base: session.BaseNow(), Message: session.AgentMessage{
						Message: result.Content,
					}})
				}

				// Add tool results to context
				a.mu.Lock()
				a.state.Messages = append(a.state.Messages, toolResults...)
				a.mu.Unlock()
				*newMessages = append(*newMessages, toolResults...)
				for _, result := range llmToolResults {
					if err := a.writeModelMessage(ctx, result); err != nil {
						return err
					}
				}
			}

			// Emit TurnEnd per-turn (Pi parity: turn_end inside the loop)
			a.emit(session.TurnEnd{
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

			turnContext := ShouldStopAfterTurnContext{
				Message:     llmMessage,
				ToolResults: agentMessagesToLLM(toolResults),
				Context:     a.buildContext(),
				NewMessages: cloneAgentMessages(*newMessages),
			}
			if a.config.PrepareNextTurn != nil {
				a.applyTurnUpdate(a.config.PrepareNextTurn(turnContext))
				turnContext.Context = a.buildContext()
			}

			// Check if we should stop after this turn
			if a.config.ShouldStopAfterTurn != nil {
				if a.config.ShouldStopAfterTurn(turnContext) {
					return nil
				}
			}

			// Get steering messages for next iteration
			pendingMessages = a.getSteeringMessages()
		}

		// Agent would stop here. Check for follow-up messages.
		followUpMessages := a.getFollowUpMessages()
		if len(followUpMessages) > 0 {
			pendingMessages = followUpMessages
			continue
		}

		// No more messages, exit
		return nil
	}
}

// streamAssistantResponse streams a response from the LLM.
func (a *Agent) streamAssistantResponse(ctx context.Context) (AgentMessage, llm.Message, error) {
	a.mu.RLock()
	config := a.config
	state := a.state
	a.mu.RUnlock()

	// Apply context transform if configured
	messages := state.Messages
	if config.TransformContext != nil {
		messages = config.TransformContext(ctx, messages)
	}

	// Convert to LLM-compatible messages
	var llmMessages []llm.Message
	if config.ConvertToLlm != nil {
		llmMessages = config.ConvertToLlm(messages)
	} else {
		llmMessages = a.defaultConvertToLlm(messages)
	}

	// Convert agent tools to LLM specs
	var toolSpecs []*llm.Spec
	if len(state.Tools) > 0 {
		toolSpecs = make([]*llm.Spec, 0, len(state.Tools))
		for _, t := range state.Tools {
			spec := &llm.Spec{
				Name:        t.Name,
				Description: t.Description,
				Parameters:  t.Parameters,
			}
			toolSpecs = append(toolSpecs, spec)
		}
	}

	// Build LLM request
	req := &llm.Request{
		Model:           config.Model.ID,
		Messages:        llmMessages,
		Tools:           toolSpecs,
		MaxTokens:       config.MaxTokens,
		Temperature:     config.Temperature,
		ReasoningEffort: string(config.ThinkingLevel),
	}

	// Stream the response
	stream, err := config.StreamFn(ctx, req)
	if err != nil {
		return AgentMessage{}, llm.Message{}, fmt.Errorf("stream: %w", err)
	}
	defer stream.Close()

	// Collect the response using StreamAccumulator.
	// Events are emitted for streaming; accumulation handles blocks + flat fields.
	var acc llm.StreamAccumulator

	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}

		if chunk.Content != "" {
			a.emit(session.AgentDelta{
				Base:      session.BaseNow(),
				Delta:     chunk.Content,
				BlockType: "text",
			})
		}
		if chunk.Reasoning != "" {
			a.emit(session.ThinkingDelta{
				Base:  session.BaseNow(),
				Delta: chunk.Reasoning,
			})
		}
		acc.Add(chunk)
	}

	if err := stream.Err(); err != nil {
		return AgentMessage{}, llm.Message{}, fmt.Errorf("stream: %w", err)
	}

	resp := acc.Response()
	var calls []AgentToolCall
	for _, call := range resp.Calls {
		calls = append(calls, AgentToolCall{
			ID:        call.ID,
			Name:      call.Function.Name,
			Arguments: parseArguments(call.Function.Arguments),
		})
	}

	message := AgentMessage{
		Role:         "assistant",
		Content:      resp.Content,
		Reasoning:    resp.Reasoning,
		Calls:        calls,
		InputTokens:  usageValue(&resp.Usage, "input"),
		OutputTokens: usageValue(&resp.Usage, "output"),
		TotalTokens:  usageValue(&resp.Usage, "total"),
		Cost:         usageValueF(&resp.Usage),
	}
	llmMessage := agentMessageToLLM(message)
	llmMessage.ThinkingBlocks = resp.ThinkingBlocks
	llmMessage.Blocks = resp.Blocks
	llmMessage.Calls = resp.Calls
	return message, llmMessage, nil
}
