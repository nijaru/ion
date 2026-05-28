package agent

import (
	"context"
	"fmt"
	"sync"

	"github.com/nijaru/ion/internal/llm"
)

// Agent is the core agent loop primitive. It manages the lifecycle of an
// agent session: submit → stream → tool calls → results → done.
type Agent struct {
	config AgentLoopConfig
	state  AgentState
	mu     sync.RWMutex
}

// New creates a new Agent with the given configuration.
func New(config AgentLoopConfig) *Agent {
	return &Agent{
		config: config,
		state: AgentState{
			Model:         config.Model,
			ThinkingLevel: config.ThinkingLevel,
			Tools:         []AgentTool{},
		},
	}
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

// Run starts the agent loop with the given prompt messages.
// It returns the new messages added during the loop.
func (a *Agent) Run(ctx context.Context, prompts []AgentMessage) ([]AgentMessage, error) {
	a.mu.Lock()
	a.state.IsStreaming = true
	a.state.ErrorMessage = ""
	a.mu.Unlock()

	defer func() {
		a.mu.Lock()
		a.state.IsStreaming = false
		a.mu.Unlock()
	}()

	// Add prompts to context
	a.mu.Lock()
	a.state.Messages = append(a.state.Messages, prompts...)
	a.mu.Unlock()

	newMessages := make([]AgentMessage, len(prompts))
	copy(newMessages, prompts)

	// Run the main loop
	err := a.runLoop(ctx, &newMessages)
	if err != nil {
		a.mu.Lock()
		a.state.ErrorMessage = err.Error()
		a.mu.Unlock()
		return newMessages, err
	}

	return newMessages, nil
}

// Continue continues the agent loop from the current context without adding new messages.
// Used for retries - context already has user message or tool results.
func (a *Agent) Continue(ctx context.Context) ([]AgentMessage, error) {
	a.mu.RLock()
	if len(a.state.Messages) == 0 {
		a.mu.RUnlock()
		return nil, fmt.Errorf("cannot continue: no messages in context")
	}
	lastMsg := a.state.Messages[len(a.state.Messages)-1]
	a.mu.RUnlock()

	if lastMsg.Role == "assistant" {
		return nil, fmt.Errorf("cannot continue from message role: assistant")
	}

	a.mu.Lock()
	a.state.IsStreaming = true
	a.state.ErrorMessage = ""
	a.mu.Unlock()

	defer func() {
		a.mu.Lock()
		a.state.IsStreaming = false
		a.mu.Unlock()
	}()

	newMessages := make([]AgentMessage, 0)

	// Run the main loop
	err := a.runLoop(ctx, &newMessages)
	if err != nil {
		a.mu.Lock()
		a.state.ErrorMessage = err.Error()
		a.mu.Unlock()
		return newMessages, err
	}

	return newMessages, nil
}

// runLoop is the main agent loop logic.
func (a *Agent) runLoop(ctx context.Context, newMessages *[]AgentMessage) error {
	// Outer loop: continues when queued follow-up messages arrive
	for {
		hasMoreToolCalls := true
		firstTurn := true

		// Check for steering messages at start
		pendingMessages := a.getSteeringMessages()

		// Inner loop: process tool calls and steering messages
		for hasMoreToolCalls || len(pendingMessages) > 0 {
			// Check for context cancellation
			if ctx.Err() != nil {
				return ctx.Err()
			}

			if !firstTurn {
				// Emit turn_start event (TODO: implement event emission)
			} else {
				firstTurn = false
			}

			// Process pending messages (inject before next assistant response)
			if len(pendingMessages) > 0 {
				a.mu.Lock()
				a.state.Messages = append(a.state.Messages, pendingMessages...)
				a.mu.Unlock()
				*newMessages = append(*newMessages, pendingMessages...)
				pendingMessages = nil
			}

			// Stream assistant response
			message, err := a.streamAssistantResponse(ctx)
			if err != nil {
				return fmt.Errorf("stream assistant response: %w", err)
			}

			a.mu.Lock()
			a.state.Messages = append(a.state.Messages, message)
			a.mu.Unlock()
			*newMessages = append(*newMessages, message)

			// Check for error/abort
			if message.IsError {
				return fmt.Errorf("assistant response error: %s", message.Content)
			}

			// Check for tool calls
			toolCalls := message.Calls
			hasMoreToolCalls = false

			if len(toolCalls) > 0 {
				toolResults, terminate, err := a.executeToolCalls(ctx, message, toolCalls)
				if err != nil {
					return fmt.Errorf("execute tool calls: %w", err)
				}

				hasMoreToolCalls = !terminate

				// Add tool results to context
				a.mu.Lock()
				a.state.Messages = append(a.state.Messages, toolResults...)
				a.mu.Unlock()
				*newMessages = append(*newMessages, toolResults...)
			}

			// Check if we should stop after this turn
			if a.config.ShouldStopAfterTurn != nil {
				ctx := ShouldStopAfterTurnContext{
					Message:     llm.Message{},
					ToolResults: []llm.Message{},
					Context:     a.buildContext(),
					NewMessages: *newMessages,
				}
				if a.config.ShouldStopAfterTurn(ctx) {
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
func (a *Agent) streamAssistantResponse(ctx context.Context) (AgentMessage, error) {
	a.mu.RLock()
	config := a.config
	state := a.state
	a.mu.RUnlock()

	// Apply context transform if configured
	messages := state.Messages
	if config.TransformContext != nil {
		messages = config.TransformContext(messages)
	}

	// Convert to LLM-compatible messages
	var llmMessages []llm.Message
	if config.ConvertToLlm != nil {
		llmMessages = config.ConvertToLlm(messages)
	} else {
		llmMessages = a.defaultConvertToLlm(messages)
	}

	// Build LLM request
	req := &llm.Request{
		Model:          config.Model.ID,
		Messages:       llmMessages,
		MaxTokens:      config.MaxTokens,
		Temperature:    config.Temperature,
		ReasoningEffort: string(config.ThinkingLevel),
	}

	// Stream the response
	stream, err := config.StreamFn(ctx, req)
	if err != nil {
		return AgentMessage{}, fmt.Errorf("stream: %w", err)
	}
	defer stream.Close()

	// Collect the response
	var content string
	var reasoning string
	var calls []AgentToolCall

	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}

		if chunk.Content != "" {
			content += chunk.Content
		}
		if chunk.Reasoning != "" {
			reasoning += chunk.Reasoning
		}
		if len(chunk.Calls) > 0 {
			for _, call := range chunk.Calls {
				calls = append(calls, AgentToolCall{
					ID:       call.ID,
					Name:     call.Function.Name,
					Arguments: parseArguments(call.Function.Arguments),
				})
			}
		}
	}

	if err := stream.Err(); err != nil {
		return AgentMessage{
			Role:    "assistant",
			Content: fmt.Sprintf("Stream error: %v", err),
			IsError: true,
		}, nil
	}

	return AgentMessage{
		Role:      "assistant",
		Content:   content,
		Reasoning: reasoning,
		Calls:     calls,
	}, nil
}

// executeToolCalls executes tool calls from an assistant message.
func (a *Agent) executeToolCalls(ctx context.Context, assistantMsg AgentMessage, toolCalls []AgentToolCall) ([]AgentMessage, bool, error) {
	a.mu.RLock()
	config := a.config
	a.mu.RUnlock()

	var results []AgentMessage
	terminate := false

	for _, toolCall := range toolCalls {
		// Check for context cancellation
		if ctx.Err() != nil {
			return results, terminate, ctx.Err()
		}

		// Call beforeToolCall hook
		if config.BeforeToolCall != nil {
			hookCtx := BeforeToolCallContext{
				AssistantMessage: llm.Message{},
				ToolCall:        toolCall,
				Context:         a.buildContext(),
			}
			result := config.BeforeToolCall(hookCtx)
			if result.Block {
				results = append(results, AgentMessage{
					Role:    "tool",
					Content: result.Reason,
					ToolID:  toolCall.ID,
					IsError: true,
				})
				continue
			}
		}

		// TODO: Actually execute the tool
		// For now, return a placeholder result
		toolResult := AgentMessage{
			Role:    "tool",
			Content: fmt.Sprintf("Tool %s executed (placeholder)", toolCall.Name),
			ToolID:  toolCall.ID,
		}

		// Call afterToolCall hook
		if config.AfterToolCall != nil {
			hookCtx := AfterToolCallContext{
				AssistantMessage: llm.Message{},
				ToolCall:        toolCall,
				Result: AgentToolResult{
					Content: []llm.ContentPart{{Type: llm.ContentPartText, Text: toolResult.Content}},
				},
				Context: a.buildContext(),
			}
			result := config.AfterToolCall(hookCtx)
			if result.Terminate != nil && *result.Terminate {
				terminate = true
			}
		}

		results = append(results, toolResult)
	}

	return results, terminate, nil
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
		Messages:     a.state.Messages,
		SystemPrompt: a.state.SystemPrompt,
		Tools:        a.state.Tools,
		Model:        a.state.Model,
		ThinkingLevel: a.state.ThinkingLevel,
	}
}

// defaultConvertToLlm converts AgentMessages to LLM Messages using default logic.
func (a *Agent) defaultConvertToLlm(messages []AgentMessage) []llm.Message {
	result := make([]llm.Message, 0, len(messages))
	for _, msg := range messages {
		llmMsg := llm.Message{
			Role:    llm.Role(msg.Role),
			Content: msg.Content,
			Name:    msg.Name,
			ToolID:  msg.ToolID,
		}

		// Convert parts
		if len(msg.Parts) > 0 {
			llmMsg.Parts = msg.Parts
		}

		// Convert tool calls
		if len(msg.Calls) > 0 {
			llmMsg.Calls = make([]llm.Call, len(msg.Calls))
			for i, call := range msg.Calls {
				llmMsg.Calls[i] = llm.Call{
					ID:   call.ID,
					Type: "function",
				}
				llmMsg.Calls[i].Function.Name = call.Name
				llmMsg.Calls[i].Function.Arguments = serializeArguments(call.Arguments)
			}
		}

		result = append(result, llmMsg)
	}
	return result
}

// parseArguments parses a JSON string into a map.
func parseArguments(args string) map[string]any {
	// TODO: Implement proper JSON parsing
	return map[string]any{"raw": args}
}

// serializeArguments serializes a map into a JSON string.
func serializeArguments(args map[string]any) string {
	// TODO: Implement proper JSON serialization
	if raw, ok := args["raw"]; ok {
		if s, ok := raw.(string); ok {
			return s
		}
	}
	return "{}"
}
