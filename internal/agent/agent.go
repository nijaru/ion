package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
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
// It returns the new messages added during the loop.
// Always emits TurnEnd when done (matching Pi's agent_end contract).
func (a *Agent) Run(ctx context.Context, prompts []AgentMessage) ([]AgentMessage, error) {
	newMessages, err := a.acceptPrompts(ctx, prompts)
	if err != nil {
		a.mu.Lock()
		a.state.ErrorMessage = err.Error()
		a.mu.Unlock()
		return newMessages, err
	}

	newMessages, runErr := a.execute(ctx, &newMessages)
	a.emit(session.TurnEnd{Base: session.BaseNow()})
	return newMessages, runErr
}

// Continue continues the agent loop without adding new messages.
// Used for retries - context already has user message or tool results.
// Does NOT emit TurnEnd (the caller owns lifecycle events).
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

	return a.execute(ctx, new([]AgentMessage))
}

// execute runs the main loop with streaming state management.
// Shared by Run and Continue.
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
				return fmt.Errorf("stream assistant response: %w", err)
			}

			a.mu.Lock()
			a.state.Messages = append(a.state.Messages, message)
			a.mu.Unlock()
			*newMessages = append(*newMessages, message)

			// Emit complete assistant message event
			a.emit(session.AgentMessage{
				Base:      session.BaseNow(),
				Message:   message.Content,
				Reasoning: message.Reasoning,
			})
			if err := a.writeModelMessage(ctx, llmMessage); err != nil {
				return err
			}

			// Check for error/abort
			if message.IsError {
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
					return fmt.Errorf("execute tool calls: %w", err)
				}

				hasMoreToolCalls = !terminate

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
		messages = config.TransformContext(messages)
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

	// Collect the response
	var content string
	var reasoning string
	var thinkingBlocks []llm.ThinkingBlock
	var calls []AgentToolCall
	var llmCalls []llm.Call

	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}

		if chunk.Content != "" {
			content += chunk.Content
			a.emit(session.AgentDelta{
				Base:  session.BaseNow(),
				Delta: chunk.Content,
			})
		}
		if chunk.Reasoning != "" {
			reasoning += chunk.Reasoning
			a.emit(session.ThinkingDelta{
				Base:  session.BaseNow(),
				Delta: chunk.Reasoning,
			})
		}
		if len(chunk.ThinkingBlocks) > 0 {
			thinkingBlocks = append(thinkingBlocks, chunk.ThinkingBlocks...)
		}
		if chunk.Usage != nil {
			a.emit(session.TokenUsage{
				Base:   session.BaseNow(),
				Input:  chunk.Usage.InputTokens,
				Output: chunk.Usage.OutputTokens,
				Total:  chunk.Usage.TotalTokens,
				Cost:   chunk.Usage.Cost,
			})
		}
		if len(chunk.Calls) > 0 {
			for _, call := range chunk.Calls {
				llmCalls = upsertLLMCall(llmCalls, call)
				calls = upsertAgentToolCall(calls, AgentToolCall{
					ID:        call.ID,
					Name:      call.Function.Name,
					Arguments: parseArguments(call.Function.Arguments),
				})
			}
		}
	}

	if err := stream.Err(); err != nil {
		return AgentMessage{}, llm.Message{}, fmt.Errorf("stream: %w", err)
	}

	message := AgentMessage{
		Role:      "assistant",
		Content:   content,
		Reasoning: reasoning,
		Calls:     calls,
	}
	llmMessage := agentMessageToLLM(message)
	llmMessage.ThinkingBlocks = thinkingBlocks
	llmMessage.Calls = llmCalls
	return message, llmMessage, nil
}

// executeToolCalls executes tool calls from an assistant message.
func (a *Agent) executeToolCalls(
	ctx context.Context,
	assistantMsg AgentMessage,
	assistantLLM llm.Message,
	toolCalls []AgentToolCall,
) ([]AgentMessage, []llm.Message, bool, error) {
	a.mu.RLock()
	config := a.config
	a.mu.RUnlock()

	if a.shouldExecuteSequentially(config, toolCalls) {
		return a.executeToolCallsSequential(ctx, assistantMsg, assistantLLM, toolCalls, config)
	}
	return a.executeToolCallsParallel(ctx, assistantMsg, assistantLLM, toolCalls, config)
}

func (a *Agent) executeToolCallsSequential(
	ctx context.Context,
	assistantMsg AgentMessage,
	assistantLLM llm.Message,
	toolCalls []AgentToolCall,
	config AgentLoopConfig,
) ([]AgentMessage, []llm.Message, bool, error) {
	finalized := make([]toolCallResult, 0, len(toolCalls))

	for _, toolCall := range toolCalls {
		// Check for context cancellation
		if ctx.Err() != nil {
			return nil, nil, false, ctx.Err()
		}

		a.emitToolCallStarted(toolCall)
		prepared := a.prepareToolCall(assistantLLM, toolCall, config)
		var result AgentToolResult
		var isError bool
		if prepared.Kind == "immediate" {
			result, isError = prepared.Result, prepared.IsError
		} else {
			result, isError = a.executePreparedToolCall(ctx, prepared, config)
			result, isError = a.finalizeExecutedToolCall(assistantLLM, prepared, result, isError, config)
		}
		message := createToolResultMessage(toolCall, result, isError)
		res := toolCallResult{
			toolCall:  toolCall,
			result:    result,
			message:   message,
			llm:       agentMessageToLLM(message),
			isError:   isError,
			terminate: result.Terminate,
		}
		a.emitToolResult(res)
		finalized = append(finalized, res)
	}

	return toolMessages(finalized)
}

func (a *Agent) executeToolCallsParallel(
	ctx context.Context,
	assistantMsg AgentMessage,
	assistantLLM llm.Message,
	toolCalls []AgentToolCall,
	config AgentLoopConfig,
) ([]AgentMessage, []llm.Message, bool, error) {
	finalized := make([]toolCallResult, len(toolCalls))
	prepared := make([]toolPreparation, len(toolCalls))

	// 1. Prepare sequentially (validate, beforeToolCall hook)
	for i, toolCall := range toolCalls {
		if ctx.Err() != nil {
			return nil, nil, false, ctx.Err()
		}
		a.emitToolCallStarted(toolCall)
		prepared[i] = a.prepareToolCall(assistantLLM, toolCall, config)

		if prepared[i].Kind == "immediate" {
			// Already resolved (not found, blocked, error)
			message := createToolResultMessage(toolCall, prepared[i].Result, prepared[i].IsError)
			finalized[i] = toolCallResult{
				toolCall:  toolCall,
				result:    prepared[i].Result,
				message:   message,
				llm:       agentMessageToLLM(message),
				isError:   prepared[i].IsError,
				terminate: prepared[i].Result.Terminate,
			}
		}
	}

	// 2. Execute and finalize prepared tools concurrently
	type execResult struct {
		idx     int
		result  AgentToolResult
		isError bool
	}
	var wg sync.WaitGroup
	results := make(chan execResult, len(toolCalls))
	for i, prep := range prepared {
		if prep.Kind != "prepared" {
			continue
		}
		wg.Add(1)
		go func(idx int, p toolPreparation) {
			defer wg.Done()
			result, isError := a.executePreparedToolCall(ctx, p, config)
			result, isError = a.finalizeExecutedToolCall(assistantLLM, p, result, isError, config)
			results <- execResult{idx, result, isError}
		}(i, prep)
	}
	wg.Wait()
	close(results)

	if err := ctx.Err(); err != nil {
		return nil, nil, false, err
	}

	// 3. Create messages and emit results in source order
	for r := range results {
		prep := prepared[r.idx]
		message := createToolResultMessage(prep.ToolCall, r.result, r.isError)
		finalized[r.idx] = toolCallResult{
			toolCall:  prep.ToolCall,
			result:    r.result,
			message:   message,
			llm:       agentMessageToLLM(message),
			isError:   r.isError,
			terminate: r.result.Terminate,
		}
	}

	for _, result := range finalized {
		a.emitToolResult(result)
	}

	return toolMessages(finalized)
}

type toolCallResult struct {
	toolCall  AgentToolCall
	result    AgentToolResult
	message   AgentMessage
	llm       llm.Message
	isError   bool
	terminate bool
}

func (a *Agent) emitToolCallStarted(toolCall AgentToolCall) {
	a.emit(session.ToolCallStart{
		Base:      session.BaseNow(),
		ToolUseID: toolCall.ID,
		ToolName:  toolCall.Name,
		Args:      serializeArguments(toolCall.Arguments),
	})
}

func (a *Agent) emitToolResult(result toolCallResult) {
	a.emit(session.ToolCallEnd{
		Base:      session.BaseNow(),
		ToolUseID: result.toolCall.ID,
		ToolName:  result.toolCall.Name,
		Result:    result.message.Content,
		Error:     toolEventError(result.message),
	})
}

// toolPreparation is the result of prepareToolCall.
// Matches Pi's discriminated union: either an immediate result or a prepared call.
type toolPreparation struct {
	// Kind is "immediate" (already resolved) or "prepared" (ready for execution).
	Kind string
	// Fields for immediate results
	Result  AgentToolResult
	IsError bool
	// Fields for prepared calls
	ToolCall AgentToolCall
	Args     any
}

// prepareToolCall validates a tool call and runs the beforeToolCall hook.
// Returns either an immediate result (tool not found, blocked, error) or a prepared call.
// Matches Pi's prepareToolCall.
func (a *Agent) prepareToolCall(
	assistantLLM llm.Message,
	toolCall AgentToolCall,
	config AgentLoopConfig,
) toolPreparation {
	if _, ok := a.findTool(toolCall.Name); !ok {
		return toolPreparation{
			Kind:    "immediate",
			Result:  errorToolResult(fmt.Sprintf("Tool %s not found", toolCall.Name)),
			IsError: true,
		}
	}
	if err := a.validateToolArgs(toolCall); err != nil {
		return toolPreparation{
			Kind:    "immediate",
			Result:  errorToolResult(fmt.Sprintf("Tool %s: invalid arguments: %v", toolCall.Name, err)),
			IsError: true,
		}
	}
	if config.BeforeToolCall != nil {
		before := config.BeforeToolCall(BeforeToolCallContext{
			AssistantMessage: assistantLLM,
			ToolCall:         toolCall,
			Args:             toolCall.Arguments,
			Context:          a.buildContext(),
		})
		if before.Block {
			reason := before.Reason
			if strings.TrimSpace(reason) == "" {
				reason = "Tool execution was blocked"
			}
			return toolPreparation{
				Kind:    "immediate",
				Result:  errorToolResult(reason),
				IsError: true,
			}
		}
	}
	return toolPreparation{
		Kind:     "prepared",
		ToolCall: toolCall,
		Args:     toolCall.Arguments,
	}
}

// executePreparedToolCall runs a prepared tool call.
// Matches Pi's executePreparedToolCall.
func (a *Agent) executePreparedToolCall(
	ctx context.Context,
	prepared toolPreparation,
	config AgentLoopConfig,
) (AgentToolResult, bool) {
	if config.ToolExecutor == nil {
		return errorToolResult(fmt.Sprintf("Tool %s executed without a configured executor", prepared.ToolCall.Name)), true
	}
	result, err := config.ToolExecutor(ctx, prepared.ToolCall)
	if err != nil {
		return errorToolResult(fmt.Sprintf("Tool execution error: %v", err)), true
	}
	if len(result.Content) == 0 {
		result.Content = []llm.ContentPart{llm.TextPart("")}
	}
	return result, result.IsError
}

// finalizeExecutedToolCall applies the afterToolCall hook.
// Matches Pi's finalizeExecutedToolCall.
func (a *Agent) finalizeExecutedToolCall(
	assistantLLM llm.Message,
	prepared toolPreparation,
	result AgentToolResult,
	isError bool,
	config AgentLoopConfig,
) (AgentToolResult, bool) {
	if config.AfterToolCall != nil {
		after := config.AfterToolCall(AfterToolCallContext{
			AssistantMessage: assistantLLM,
			ToolCall:         prepared.ToolCall,
			Args:             prepared.Args,
			Result:           result,
			IsError:          isError,
			Context:          a.buildContext(),
		})
		if after.Content != nil {
			result.Content = after.Content
		}
		if after.Details != nil {
			result.Details = after.Details
		}
		if after.IsError != nil {
			result.IsError = *after.IsError
			isError = *after.IsError
		}
		if after.Terminate != nil {
			result.Terminate = *after.Terminate
		}
	}
	return result, isError
}

// createToolResultMessage creates the tool result message.
// Matches Pi's createToolResultMessage.
func createToolResultMessage(toolCall AgentToolCall, result AgentToolResult, isError bool) AgentMessage {
	parts := normalizeContentParts(result.Content)
	text := contentPartsText(parts)
	return AgentMessage{
		Role:    "tool",
		Content: text,
		Parts:   parts,
		ToolID:  toolCall.ID,
		Name:    toolCall.Name,
		IsError: isError,
	}
}

// prepareAndExecuteTool runs the full tool lifecycle: prepare → execute → finalize.
// Used by sequential execution. For parallel, use prepareToolCall + executePreparedToolCall + finalizeExecutedToolCall.
func (a *Agent) prepareAndExecuteTool(
	ctx context.Context,
	assistantLLM llm.Message,
	toolCall AgentToolCall,
	config AgentLoopConfig,
) (AgentToolResult, bool) {
	prepared := a.prepareToolCall(assistantLLM, toolCall, config)
	if prepared.Kind == "immediate" {
		return prepared.Result, prepared.IsError
	}
	result, isError := a.executePreparedToolCall(ctx, prepared, config)
	return a.finalizeExecutedToolCall(assistantLLM, prepared, result, isError, config)
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

// defaultConvertToLlm converts AgentMessages to LLM Messages using default logic.
func (a *Agent) defaultConvertToLlm(messages []AgentMessage) []llm.Message {
	a.mu.RLock()
	systemPrompt := a.state.SystemPrompt
	var caps *llm.Capabilities
	if a.config.Model.Capabilities != nil {
		caps = a.config.Model.Capabilities
	}
	a.mu.RUnlock()

	result := make([]llm.Message, 0, len(messages)+1)

	// Prepend system prompt if set and not already present at the head of messages.
	if systemPrompt != "" {
		hasSystem := false
		if len(messages) > 0 {
			firstRole := messages[0].Role
			if firstRole == "system" || firstRole == "developer" {
				hasSystem = true
			}
		}
		if !hasSystem {
			role := llm.RoleSystem
			if caps != nil && caps.SystemRole != "" {
				role = llm.Role(caps.SystemRole)
			}
			result = append(result, llm.Message{
				Role:    role,
				Content: systemPrompt,
			})
		}
	}

	for _, msg := range messages {
		result = append(result, agentMessageToLLM(msg))
	}
	return result
}

func (a *Agent) shouldExecuteSequentially(config AgentLoopConfig, calls []AgentToolCall) bool {
	if config.ToolExecutionMode == ToolExecutionSequential {
		return true
	}
	return !a.allToolsParallel(calls)
}

func (a *Agent) allToolsParallel(calls []AgentToolCall) bool {
	for _, call := range calls {
		tool, ok := a.findTool(call.Name)
		if !ok || !tool.Parallel {
			return false
		}
	}
	return true
}

func (a *Agent) findTool(name string) (AgentTool, bool) {
	a.mu.RLock()
	defer a.mu.RUnlock()
	for _, tool := range a.state.Tools {
		if tool.Name == name {
			return tool, true
		}
	}
	return AgentTool{}, false
}

func toolMessages(finalized []toolCallResult) ([]AgentMessage, []llm.Message, bool, error) {
	messages := make([]AgentMessage, 0, len(finalized))
	llmMessages := make([]llm.Message, 0, len(finalized))
	terminate := len(finalized) > 0
	for _, result := range finalized {
		messages = append(messages, result.message)
		llmMessages = append(llmMessages, result.llm)
		terminate = terminate && result.terminate
	}
	return messages, llmMessages, terminate, nil
}

func errorToolResult(message string) AgentToolResult {
	return AgentToolResult{
		Content: []llm.ContentPart{llm.TextPart(message)},
		IsError: true,
	}
}

func toolEventError(message AgentMessage) error {
	if !message.IsError {
		return nil
	}
	if strings.TrimSpace(message.Content) == "" {
		return fmt.Errorf("tool execution failed")
	}
	return fmt.Errorf("%s", message.Content)
}

func agentMessagesToLLM(messages []AgentMessage) []llm.Message {
	result := make([]llm.Message, 0, len(messages))
	for _, message := range messages {
		result = append(result, agentMessageToLLM(message))
	}
	return result
}

func agentMessageToLLM(message AgentMessage) llm.Message {
	role := llm.Role(message.Role)
	if role == "toolResult" {
		role = llm.RoleTool
	}
	llmMessage := llm.Message{
		Role:      role,
		Content:   message.Content,
		Parts:     normalizeContentParts(message.Parts),
		Reasoning: message.Reasoning,
		Name:      message.Name,
		ToolID:    message.ToolID,
	}
	if len(message.Calls) > 0 {
		llmMessage.Calls = make([]llm.Call, len(message.Calls))
		for i, call := range message.Calls {
			llmMessage.Calls[i] = agentToolCallToLLM(call)
		}
	}
	return llmMessage
}

func agentToolCallToLLM(call AgentToolCall) llm.Call {
	var llmCall llm.Call
	llmCall.ID = call.ID
	llmCall.Type = "function"
	llmCall.Function.Name = call.Name
	llmCall.Function.Arguments = serializeArguments(call.Arguments)
	return llmCall
}

func agentMessageFromLLM(message llm.Message) AgentMessage {
	role := string(message.Role)
	if message.Role == llm.RoleTool {
		role = "tool"
	}
	result := AgentMessage{
		Role:      role,
		Content:   message.TextContent(),
		Parts:     normalizeContentParts(message.Parts),
		Reasoning: message.Reasoning,
		Name:      message.Name,
		ToolID:    message.ToolID,
	}
	if len(message.Calls) > 0 {
		result.Calls = make([]AgentToolCall, len(message.Calls))
		for i, call := range message.Calls {
			result.Calls[i] = AgentToolCall{
				ID:        call.ID,
				Name:      call.Function.Name,
				Arguments: parseArguments(call.Function.Arguments),
			}
		}
	}
	return result
}

func normalizeContentParts(parts []llm.ContentPart) []llm.ContentPart {
	if len(parts) == 0 {
		return nil
	}
	result := make([]llm.ContentPart, 0, len(parts))
	for _, part := range parts {
		if part.Type == "" {
			part.Type = llm.ContentPartText
		}
		result = append(result, part)
	}
	return result
}

func contentPartsText(parts []llm.ContentPart) string {
	var sb strings.Builder
	for _, part := range parts {
		switch part.Type {
		case "", llm.ContentPartText:
			sb.WriteString(part.Text)
		case llm.ContentPartImage:
			if sb.Len() > 0 {
				sb.WriteByte('\n')
			}
			if part.MIMEType != "" {
				sb.WriteString("Image: ")
				sb.WriteString(part.MIMEType)
			} else {
				sb.WriteString("Image")
			}
		}
	}
	return sb.String()
}

func isEmptyModelMessage(message llm.Message) bool {
	return strings.TrimSpace(message.TextContent()) == "" &&
		strings.TrimSpace(message.Reasoning) == "" &&
		len(message.ThinkingBlocks) == 0 &&
		len(message.Calls) == 0 &&
		len(message.Parts) == 0
}

func cloneAgentMessages(messages []AgentMessage) []AgentMessage {
	if len(messages) == 0 {
		return nil
	}
	result := make([]AgentMessage, len(messages))
	copy(result, messages)
	for i := range result {
		result[i].Parts = append([]llm.ContentPart(nil), result[i].Parts...)
		result[i].Calls = append([]AgentToolCall(nil), result[i].Calls...)
	}
	return result
}

func upsertLLMCall(calls []llm.Call, call llm.Call) []llm.Call {
	if call.ID == "" {
		return calls
	}
	for i := range calls {
		if calls[i].ID == call.ID {
			calls[i] = call
			return calls
		}
	}
	return append(calls, call)
}

func upsertAgentToolCall(calls []AgentToolCall, call AgentToolCall) []AgentToolCall {
	if call.ID == "" {
		return calls
	}
	for i := range calls {
		if calls[i].ID == call.ID {
			calls[i] = call
			return calls
		}
	}
	return append(calls, call)
}

// parseArguments parses a JSON string into a map.
func parseArguments(args string) map[string]any {
	var m map[string]any
	if err := json.Unmarshal([]byte(args), &m); err != nil {
		return map[string]any{"raw": args}
	}
	return m
}

// serializeArguments serializes a map into a JSON string.
func serializeArguments(args map[string]any) string {
	if args == nil {
		return "{}"
	}
	if raw, ok := args["raw"]; ok {
		if s, ok := raw.(string); ok {
			return s
		}
	}
	data, err := json.Marshal(args)
	if err != nil {
		return "{}"
	}
	return string(data)
}

// validateToolArgs validates tool arguments against the tool's parameter schema.
// Returns nil if validation passes or the tool has no schema.
func (a *Agent) validateToolArgs(toolCall AgentToolCall) error {
	tool, ok := a.findTool(toolCall.Name)
	if !ok {
		return nil // tool not found is handled separately
	}
	schema := tool.Parameters
	if schema == nil {
		return nil // no schema means no validation
	}

	// Extract required fields from schema
	schemaMap, ok := schema.(map[string]any)
	if !ok {
		return nil // non-map schemas are not validated
	}

	required, ok := schemaMap["required"]
	if !ok {
		return nil
	}
	requiredList, ok := required.([]any)
	if !ok {
		return nil
	}

	// Check required fields
	for _, field := range requiredList {
		fieldName, ok := field.(string)
		if !ok {
			continue
		}
		if _, exists := toolCall.Arguments[fieldName]; !exists {
			return fmt.Errorf("missing required field: %s", fieldName)
		}
	}

	return nil
}
