package agent

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// streamAssistantResponse streams a response from the LLM and returns
// the assistant message in both AgentMessage and llm.Message forms.
//
// It:
//  1. Applies context transform if configured
//  2. Converts messages to LLM format
//  3. Builds the LLM request
//  4. Streams the response, emitting deltas
//  5. Accumulates the response into an assistant message
//
// Returns: AgentMessage (agent's representation), llm.Message (LLM's representation), error.
func (l *AgentLoop) streamAssistantResponse(ctx context.Context) (AgentMessage, llm.Message, error) {
	// Apply context transform if configured
	messages := l.state.Messages
	if l.config.TransformContext != nil {
		messages = l.config.TransformContext(ctx, messages)
	}

	// Convert to LLM-compatible messages
	var llmMessages []llm.Message
	if l.config.ConvertToLlm != nil {
		llmMessages = l.config.ConvertToLlm(messages)
	} else {
		llmMessages = l.defaultConvertToLlm(messages)
	}

	// Convert agent tools to LLM specs
	var toolSpecs []*llm.Spec
	if len(l.state.Tools) > 0 {
		toolSpecs = make([]*llm.Spec, 0, len(l.state.Tools))
		for _, t := range l.state.Tools {
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
		Model:           l.state.Model.ID,
		Messages:        llmMessages,
		Tools:           toolSpecs,
		MaxTokens:       l.config.MaxTokens,
		Temperature:     l.config.Temperature,
		ReasoningEffort: string(l.config.ThinkingLevel),
	}

	// Call beforeProviderRequest hook (Pi parity)
	if l.config.BeforeProviderRequest != nil {
		hookCtx := BeforeProviderRequestContext{
			Model:    l.state.Model,
			Messages: llmMessages,
			Tools:    toolSpecs,
		}
		result := l.config.BeforeProviderRequest(ctx, hookCtx)
		if result.Abort {
			return AgentMessage{}, llm.Message{}, fmt.Errorf("aborted by beforeProviderRequest hook: %s", result.Reason)
		}
	}

	// Call beforeProviderPayload hook (Pi parity)
	if l.config.BeforeProviderPayload != nil {
		hookCtx := BeforeProviderPayloadContext{
			Model:   l.state.Model,
			Payload: req,
		}
		l.config.BeforeProviderPayload(ctx, hookCtx)
	}

	// Stream the response
	stream, err := l.config.StreamFn(ctx, req)
	if err != nil {
		return AgentMessage{}, llm.Message{}, fmt.Errorf("stream: %w", err)
	}
	defer stream.Close()

	// Collect the response using StreamAccumulator.
	// Events are emitted for streaming; accumulation handles blocks + flat fields.
	var acc llm.StreamAccumulator

	// Pi parity: push a partial assistant message to context.messages at stream start,
	// then update it in-place for each chunk. This makes the partial message visible
	// to hooks that read context.messages during streaming.
	partialIdx := len(l.state.Messages)
	l.state.Messages = append(l.state.Messages, AgentMessage{Role: "assistant"})
	partialSessionMsg := session.AgentMessage{}

	// Track content block state for structured events (Pi parity)
	var currentBlockType string // "text", "thinking", "tool_call"
	var currentBlockIndex int
	var currentToolID string
	var currentToolName string
	var toolCallStarted bool

	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}

		// Detect content block transitions for structured events
		if chunk.Block != nil {
			switch b := chunk.Block.(type) {
			case llm.TextBlock:
				if currentBlockType != "text" {
					if currentBlockType != "" {
						emitBlockEnd(l, currentBlockType, currentBlockIndex, currentToolID, currentToolName, partialSessionMsg)
					}
					currentBlockType = "text"
					currentBlockIndex = len(acc.Response().Blocks)
					l.emit(session.TextStart{
						Base:         session.BaseNow(),
						ContentIndex: currentBlockIndex,
						Partial:      partialSessionMsg,
					})
				}
			case llm.ThinkingBlock:
				if currentBlockType != "thinking" {
					if currentBlockType != "" {
						emitBlockEnd(l, currentBlockType, currentBlockIndex, currentToolID, currentToolName, partialSessionMsg)
					}
					currentBlockType = "thinking"
					currentBlockIndex = len(acc.Response().Blocks)
					l.emit(session.ThinkingStart{
						Base:         session.BaseNow(),
						ContentIndex: currentBlockIndex,
						Partial:      partialSessionMsg,
					})
				}
			case llm.ToolCallBlock:
				if currentBlockType != "tool_call" || b.ID != currentToolID {
					if currentBlockType != "" {
						emitBlockEnd(l, currentBlockType, currentBlockIndex, currentToolID, currentToolName, partialSessionMsg)
					}
					currentBlockType = "tool_call"
					currentBlockIndex = len(acc.Response().Blocks)
					currentToolID = b.ID
					currentToolName = b.Name
					toolCallStarted = false
				}
				if !toolCallStarted {
					l.emit(session.ToolCallBlockStart{
						Base:         session.BaseNow(),
						ContentIndex: currentBlockIndex,
						ToolUseID:    b.ID,
						ToolName:     b.Name,
						Partial:      partialSessionMsg,
					})
					toolCallStarted = true
				}
			}
		}

		// Emit structured deltas + backward-compatible MessageUpdate
		if chunk.Content != "" {
			partialSessionMsg.Message += chunk.Content
			l.emit(session.TextDelta{
				Base:         session.BaseNow(),
				ContentIndex: currentBlockIndex,
				Delta:        chunk.Content,
				Partial:      partialSessionMsg,
			})
			// Backward-compatible MessageUpdate
			l.emit(session.MessageUpdate{
				Base:      session.BaseNow(),
				Message:   partialSessionMsg,
				Delta:     chunk.Content,
				BlockType: "text",
			})
		}
		if chunk.Reasoning != "" {
			partialSessionMsg.Reasoning += chunk.Reasoning
			l.emit(session.ThinkingDelta{
				Base:         session.BaseNow(),
				ContentIndex: currentBlockIndex,
				Delta:        chunk.Reasoning,
				Partial:      partialSessionMsg,
			})
			// Backward-compatible MessageUpdate
			l.emit(session.MessageUpdate{
				Base:      session.BaseNow(),
				Message:   partialSessionMsg,
				Delta:     chunk.Reasoning,
				BlockType: "thinking",
			})
		}
		for _, call := range chunk.Calls {
			l.emit(session.ToolCallBlockDelta{
				Base:         session.BaseNow(),
				ContentIndex: currentBlockIndex,
				ToolUseID:    call.ID,
				Delta:        call.Function.Arguments,
				Partial:      partialSessionMsg,
			})
		}
		acc.Add(chunk)

		// Update the partial message in-place in context.messages (Pi parity)
		resp := acc.Response()
		l.state.Messages[partialIdx] = AgentMessage{
			Role:         "assistant",
			Parts:        respParts(resp),
			InputTokens:  usageValue(&resp.Usage, "input"),
			OutputTokens: usageValue(&resp.Usage, "output"),
			TotalTokens:  usageValue(&resp.Usage, "total"),
			Cost:         usageValueF(&resp.Usage),
		}
	}

	// Emit final block end
	if currentBlockType != "" {
		emitBlockEnd(l, currentBlockType, currentBlockIndex, currentToolID, currentToolName, partialSessionMsg)
	}

	if err := stream.Err(); err != nil {
		// Remove the partial message on error
		l.state.Messages = l.state.Messages[:partialIdx]
		// Call afterProviderResponse hook on error (Pi parity)
		if l.config.AfterProviderResponse != nil {
			l.config.AfterProviderResponse(ctx, AfterProviderResponseContext{
				Model: l.state.Model,
				Error: err,
			})
		}
		return AgentMessage{}, llm.Message{}, fmt.Errorf("stream: %w", err)
	}

	resp := acc.Response()

	// Call afterProviderResponse hook on success (Pi parity)
	if l.config.AfterProviderResponse != nil {
		l.config.AfterProviderResponse(ctx, AfterProviderResponseContext{
			Model:    l.state.Model,
			Response: &resp,
		})
	}
	var calls []AgentToolCall
	for _, call := range resp.ToolCalls() {
		calls = append(calls, AgentToolCall{
			ID:        call.ID,
			Name:      call.Function.Name,
			Arguments: parseArguments(call.Function.Arguments),
		})
	}

	message := AgentMessage{
		Role:         "assistant",
		Parts:        respParts(resp),
		Calls:        calls,
		InputTokens:  usageValue(&resp.Usage, "input"),
		OutputTokens: usageValue(&resp.Usage, "output"),
		TotalTokens:  usageValue(&resp.Usage, "total"),
		Cost:         usageValueF(&resp.Usage),
	}
	// Replace the partial message with the final message
	l.state.Messages[partialIdx] = message

	// Add assistant message to tree store
	l.treeMu.Lock()
	var parentID *string
	if leaf := l.tree.Leaf(); leaf != nil {
		id := leaf.ID
			parentID = &id
		}
		llmMsg := agentMessageToLLM(message)
		l.addToTreeLocked(parentID, &llmMsg)
	l.treeMu.Unlock()
	llmMessage := agentMessageToLLM(message)
	llmMessage.Blocks = resp.GetContentBlocks()
	return message, llmMessage, nil
}

// defaultConvertToLlm converts AgentMessages to LLM Messages using default logic.
func (l *AgentLoop) defaultConvertToLlm(messages []AgentMessage) []llm.Message {
	var caps *llm.Capabilities
	if l.config.Model.Capabilities != nil {
		caps = l.config.Model.Capabilities
	}

	result := make([]llm.Message, 0, len(messages)+1)

	// Prepend system prompt if set and not already present
	if l.state.SystemPrompt != "" {
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
				Content: l.state.SystemPrompt,
			})
		}
	}

	for _, msg := range messages {
		result = append(result, agentMessageToLLM(msg))
	}
	return result
}

// respParts converts an llm.Response to ContentParts for AgentMessage.
func respParts(resp llm.Response) []llm.ContentPart {
	var parts []llm.ContentPart
	for _, block := range resp.GetContentBlocks() {
		switch v := block.(type) {
		case llm.TextBlock:
			parts = append(parts, llm.ContentPart{Type: llm.ContentPartText, Text: v.Text})
		case llm.ThinkingBlock:
			parts = append(parts, llm.ContentPart{Type: "reasoning", Text: v.Thinking})
		}
	}
	return parts
}

// emitBlockEnd emits the appropriate end event for a content block.
func emitBlockEnd(l *AgentLoop, blockType string, index int, toolID, toolName string, partial session.AgentMessage) {
	switch blockType {
	case "text":
		l.emit(session.TextEnd{
			Base:         session.BaseNow(),
			ContentIndex: index,
			Content:      partial.Message,
			Partial:      partial,
		})
	case "thinking":
		l.emit(session.ThinkingEnd{
			Base:         session.BaseNow(),
			ContentIndex: index,
			Content:      partial.Reasoning,
			Partial:      partial,
		})
	case "tool_call":
		l.emit(session.ToolCallBlockEnd{
			Base:         session.BaseNow(),
			ContentIndex: index,
			ToolUseID:    toolID,
			ToolName:     toolName,
			Partial:      partial,
		})
	}
}
