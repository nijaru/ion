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
//
// It:
//  1. Determines execution mode (sequential or parallel)
//  2. Prepares each tool call (validate, beforeToolCall hook)
//  3. Executes prepared tool calls
//  4. Finalizes results (afterToolCall hook)
//  5. Emits tool execution events
//
// Returns: tool result messages, LLM tool result messages, terminate hint, error.
func (l *AgentLoop) executeToolCalls(
	ctx context.Context,
	assistantMsg AgentMessage,
	assistantLLM llm.Message,
	toolCalls []AgentToolCall,
) ([]AgentMessage, []llm.Message, bool, error) {
	if l.shouldExecuteSequentially(toolCalls) {
		return l.executeToolCallsSequential(ctx, assistantMsg, assistantLLM, toolCalls)
	}
	return l.executeToolCallsParallel(ctx, assistantMsg, assistantLLM, toolCalls)
}

func (l *AgentLoop) executeToolCallsSequential(
	ctx context.Context,
	assistantMsg AgentMessage,
	assistantLLM llm.Message,
	toolCalls []AgentToolCall,
) ([]AgentMessage, []llm.Message, bool, error) {
	finalized := make([]toolCallResult, 0, len(toolCalls))

	for _, toolCall := range toolCalls {
		if ctx.Err() != nil {
			return nil, nil, false, ctx.Err()
		}

		l.emitToolCallStarted(toolCall)
		prepared := l.prepareToolCall(ctx, assistantLLM, toolCall)
		var result AgentToolResult
		var isError bool
		if prepared.Kind == "immediate" {
			result, isError = prepared.Result, prepared.IsError
		} else {
			result, isError = l.executePreparedToolCall(ctx, prepared)
			result, isError = l.finalizeExecutedToolCall(ctx, assistantLLM, prepared, result, isError)
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
		l.emitToolResult(res)
		finalized = append(finalized, res)
	}

	return toolMessages(finalized)
}

func (l *AgentLoop) executeToolCallsParallel(
	ctx context.Context,
	assistantMsg AgentMessage,
	assistantLLM llm.Message,
	toolCalls []AgentToolCall,
) ([]AgentMessage, []llm.Message, bool, error) {
	finalized := make([]toolCallResult, len(toolCalls))
	prepared := make([]toolPreparation, len(toolCalls))

	// 1. Prepare sequentially (validate, beforeToolCall hook)
	for i, toolCall := range toolCalls {
		if ctx.Err() != nil {
			return nil, nil, false, ctx.Err()
		}
		l.emitToolCallStarted(toolCall)
		prepared[i] = l.prepareToolCall(ctx, assistantLLM, toolCall)

		if prepared[i].Kind == "immediate" {
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
			result, isError := l.executePreparedToolCall(ctx, p)
			result, isError = l.finalizeExecutedToolCall(ctx, assistantLLM, p, result, isError)
			results <- execResult{idx, result, isError}
		}(i, prep)
	}
	wg.Wait()
	close(results)

	if err := ctx.Err(); err != nil {
		return nil, nil, false, err
	}

	// 3. Create messages and emit results in completion order
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
		// Emit tool result as it completes (Pi parity: tool_execution_end in completion order)
		l.emitToolResult(finalized[r.idx])
	}

	return toolMessages(finalized)
}

// toolCallResult holds the result of executing a single tool call.
type toolCallResult struct {
	toolCall  AgentToolCall
	result    AgentToolResult
	message   AgentMessage
	llm       llm.Message
	isError   bool
	terminate bool
}

// toolPreparation is the result of prepareToolCall.
// Discriminated union: either an immediate result or a prepared call.
type toolPreparation struct {
	Kind     string // "immediate" or "prepared"
	Result   AgentToolResult
	IsError  bool
	ToolCall AgentToolCall
	Args     any
}

func (l *AgentLoop) emitToolCallStarted(toolCall AgentToolCall) {
	l.emit(session.ToolCallStart{
		Base:      session.BaseNow(),
		ToolUseID: toolCall.ID,
		ToolName:  toolCall.Name,
		Args:      serializeArguments(toolCall.Arguments),
	})
}

func (l *AgentLoop) emitToolResult(result toolCallResult) {
	l.emit(session.ToolCallEnd{
		Base:      session.BaseNow(),
		ToolUseID: result.toolCall.ID,
		ToolName:  result.toolCall.Name,
		Result:    result.message.TextContent(),
		Error:     toolEventError(result.message),
	})
}

// prepareToolCall validates a tool call and runs the beforeToolCall hook.
// Returns either an immediate result (tool not found, blocked, error) or a prepared call.
func (l *AgentLoop) prepareToolCall(
	ctx context.Context,
	assistantLLM llm.Message,
	toolCall AgentToolCall,
) toolPreparation {
	tool, ok := l.findTool(toolCall.Name)
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
	if err := l.validateToolArgs(toolCall); err != nil {
		return toolPreparation{
			Kind:    "immediate",
			Result:  errorToolResult(fmt.Sprintf("Tool %s: invalid arguments: %v", toolCall.Name, err)),
			IsError: true,
		}
	}
	if l.config.BeforeToolCall != nil {
		before := l.config.BeforeToolCall(ctx, BeforeToolCallContext{
			AssistantMessage: assistantLLM,
			ToolCall:         toolCall,
			Args:             toolCall.Arguments,
			Context:          l.buildContext(),
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
func (l *AgentLoop) executePreparedToolCall(
	ctx context.Context,
	prepared toolPreparation,
) (AgentToolResult, bool) {
	if l.config.ToolExecutor == nil {
		return errorToolResult(fmt.Sprintf("Tool %s executed without a configured executor", prepared.ToolCall.Name)), true
	}
	result, err := l.config.ToolExecutor(ctx, prepared.ToolCall)
	if err != nil {
		return errorToolResult(fmt.Sprintf("Tool execution error: %v", err)), true
	}
	if len(result.Content) == 0 {
		result.Content = []llm.ContentPart{llm.TextPart("")}
	}
	return result, result.IsError
}

// finalizeExecutedToolCall applies the afterToolCall hook.
func (l *AgentLoop) finalizeExecutedToolCall(
	ctx context.Context,
	assistantLLM llm.Message,
	prepared toolPreparation,
	result AgentToolResult,
	isError bool,
) (AgentToolResult, bool) {
	if l.config.AfterToolCall != nil {
		after := l.config.AfterToolCall(ctx, AfterToolCallContext{
			AssistantMessage: assistantLLM,
			ToolCall:         prepared.ToolCall,
			Args:             prepared.Args,
			Result:           result,
			IsError:          isError,
			Context:          l.buildContext(),
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
func createToolResultMessage(toolCall AgentToolCall, result AgentToolResult, isError bool) AgentMessage {
	parts := normalizeContentParts(result.Content)
	return AgentMessage{
		Role:   "tool",
		Parts:  parts,
		ToolID: toolCall.ID,
		Name:   toolCall.Name,
		IsError: isError,
	}
}

func (l *AgentLoop) shouldExecuteSequentially(calls []AgentToolCall) bool {
	for _, call := range calls {
		tool, ok := l.findTool(call.Name)
		if !ok {
			return true // unknown tool: sequential for safety
		}
		mode := tool.ExecutionMode
		if mode == "" {
			mode = l.config.ToolExecutionMode
		}
		if mode == ToolExecutionSequential {
			return true
		}
	}
	return false
}

func (l *AgentLoop) findTool(name string) (AgentTool, bool) {
	for _, tool := range l.state.Tools {
		if tool.Name == name {
			return tool, true
		}
	}
	return AgentTool{}, false
}

func toolMessages(finalized []toolCallResult) ([]AgentMessage, []llm.Message, bool, error) {
	messages := make([]AgentMessage, 0, len(finalized))
	llmMessages := make([]llm.Message, 0, len(finalized))
	terminate := false
	for _, result := range finalized {
		messages = append(messages, result.message)
		llmMessages = append(llmMessages, result.llm)
		if result.terminate {
			terminate = true
		}
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
	if strings.TrimSpace(message.TextContent()) == "" {
		return fmt.Errorf("tool execution failed")
	}
	return fmt.Errorf("%s", message.TextContent())
}

// validateToolArgs validates tool arguments against the tool's parameter schema.
func (l *AgentLoop) validateToolArgs(toolCall AgentToolCall) error {
	tool, ok := l.findTool(toolCall.Name)
	if !ok {
		return nil
	}
	schema := tool.Parameters
	if schema == nil {
		return nil
	}

	schemaMap, ok := schema.(map[string]any)
	if !ok {
		return nil
	}

	if err := validateRequired(toolCall.Arguments, schemaMap); err != nil {
		return err
	}

	return validatePropertyTypes(toolCall.Arguments, schemaMap)
}

func validateRequired(args map[string]any, schemaMap map[string]any) error {
	required, ok := schemaMap["required"]
	if !ok {
		return nil
	}
	requiredList, ok := required.([]any)
	if !ok {
		return nil
	}

	for _, field := range requiredList {
		fieldName, ok := field.(string)
		if !ok {
			continue
		}
		if _, exists := args[fieldName]; !exists {
			return fmt.Errorf("missing required field: %s", fieldName)
		}
	}

	return nil
}

func validatePropertyTypes(args map[string]any, schemaMap map[string]any) error {
	properties, ok := schemaMap["properties"]
	if !ok {
		return nil
	}
	propMap, ok := properties.(map[string]any)
	if !ok {
		return nil
	}

	for key, value := range args {
		propSchema, exists := propMap[key]
		if !exists {
			continue
		}
		pSchema, ok := propSchema.(map[string]any)
		if !ok {
			continue
		}
		expectedType, ok := pSchema["type"].(string)
		if !ok {
			continue
		}
		if err := checkValueType(value, expectedType); err != nil {
			return fmt.Errorf("field %s: %w", key, err)
		}
	}

	return nil
}

func checkValueType(value any, expectedType string) error {
	switch expectedType {
	case "string":
		if _, ok := value.(string); !ok {
			return fmt.Errorf("expected string, got %T", value)
		}
	case "number", "integer":
		if _, ok := value.(float64); !ok {
			return fmt.Errorf("expected number, got %T", value)
		}
	case "boolean":
		if _, ok := value.(bool); !ok {
			return fmt.Errorf("expected boolean, got %T", value)
		}
	case "array":
		if _, ok := value.([]any); !ok {
			return fmt.Errorf("expected array, got %T", value)
		}
	case "object":
		if _, ok := value.(map[string]any); !ok {
			return fmt.Errorf("expected object, got %T", value)
		}
	}
	return nil
}
