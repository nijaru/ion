package agent

import (
	"context"
	"fmt"
	"strings"
	"sync"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

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
		prepared := a.prepareToolCall(ctx, assistantLLM, toolCall, config)
		var result AgentToolResult
		var isError bool
		if prepared.Kind == "immediate" {
			result, isError = prepared.Result, prepared.IsError
		} else {
			result, isError = a.executePreparedToolCall(ctx, prepared, config)
			result, isError = a.finalizeExecutedToolCall(ctx, assistantLLM, prepared, result, isError, config)
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
		prepared[i] = a.prepareToolCall(ctx, assistantLLM, toolCall, config)

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
			result, isError = a.finalizeExecutedToolCall(ctx, assistantLLM, p, result, isError, config)
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
	ctx context.Context,
	assistantLLM llm.Message,
	toolCall AgentToolCall,
	config AgentLoopConfig,
) toolPreparation {
	tool, ok := a.findTool(toolCall.Name)
	if !ok {
		return toolPreparation{
			Kind:    "immediate",
			Result:  errorToolResult(fmt.Sprintf("Tool %s not found", toolCall.Name)),
			IsError: true,
		}
	}
	if tool.PrepareArguments != nil {
		toolCall.Arguments = tool.PrepareArguments(toolCall.Arguments)
	}
	if err := a.validateToolArgs(toolCall); err != nil {
		return toolPreparation{
			Kind:    "immediate",
			Result:  errorToolResult(fmt.Sprintf("Tool %s: invalid arguments: %v", toolCall.Name, err)),
			IsError: true,
		}
	}
	if config.BeforeToolCall != nil {
		before := config.BeforeToolCall(ctx, BeforeToolCallContext{
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
	ctx context.Context,
	assistantLLM llm.Message,
	prepared toolPreparation,
	result AgentToolResult,
	isError bool,
	config AgentLoopConfig,
) (AgentToolResult, bool) {
	if config.AfterToolCall != nil {
		after := config.AfterToolCall(ctx, AfterToolCallContext{
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
	prepared := a.prepareToolCall(ctx, assistantLLM, toolCall, config)
	if prepared.Kind == "immediate" {
		return prepared.Result, prepared.IsError
	}
	result, isError := a.executePreparedToolCall(ctx, prepared, config)
	return a.finalizeExecutedToolCall(ctx, assistantLLM, prepared, result, isError, config)
}

func (a *Agent) shouldExecuteSequentially(config AgentLoopConfig, calls []AgentToolCall) bool {
	for _, call := range calls {
		tool, ok := a.findTool(call.Name)
		if !ok {
			return true // unknown tool: sequential for safety
		}
		mode := tool.ExecutionMode
		if mode == "" {
			mode = config.ToolExecutionMode
		}
		if mode == ToolExecutionSequential {
			return true
		}
	}
	return false
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
