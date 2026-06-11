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

	// Stream the response
	stream, err := l.config.StreamFn(ctx, req)
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
			l.emit(session.AgentDelta{
				Base:      session.BaseNow(),
				Delta:     chunk.Content,
				BlockType: "text",
			})
		}
		if chunk.Reasoning != "" {
			l.emit(session.ThinkingDelta{
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
