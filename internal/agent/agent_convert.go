package agent

import (
	"encoding/json"
	"strings"

	"github.com/nijaru/ion/llm"
)

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
		Role:  role,
		Parts: normalizeContentParts(message.Parts),
		Name:  message.Name,
		ToolID: message.ToolID,
	}
	// Build Blocks from Parts and Calls.
	if len(message.Calls) > 0 {
		llmMessage.Calls = make([]llm.Call, len(message.Calls))
		llmMessage.Blocks = make(llm.ContentBlocks, 0, len(message.Calls))
		for i, call := range message.Calls {
			llmMessage.Calls[i] = agentToolCallToLLM(call)
			llmMessage.Blocks = append(llmMessage.Blocks, llm.ToolCallBlock{
				ID:        call.ID,
				Name:      call.Name,
				Arguments: serializeArguments(call.Arguments),
			})
		}
	} else if len(message.Parts) > 0 {
		llmMessage.Blocks = make(llm.ContentBlocks, 0, len(message.Parts))
		for _, part := range message.Parts {
			if part.Type == "reasoning" {
				llmMessage.Blocks = append(llmMessage.Blocks, llm.ThinkingBlock{Thinking: part.Text})
			} else {
				llmMessage.Blocks = append(llmMessage.Blocks, llm.TextBlock{Text: part.Text})
			}
		}
	}
	// Populate flat fields from Blocks for backward compatibility.
	llmMessage.Content = llmMessage.TextContent()
	llmMessage.Reasoning = llmMessage.BlocksReasoning()
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
		Role:   role,
		Parts:  normalizeContentParts(message.Parts),
		Name:   message.Name,
		ToolID: message.ToolID,
	}
	// Build Parts from Blocks if Parts is empty.
	if len(result.Parts) == 0 && len(message.Blocks) > 0 {
		for _, block := range message.Blocks {
			switch v := block.(type) {
			case llm.TextBlock:
				result.Parts = append(result.Parts, llm.ContentPart{Type: llm.ContentPartText, Text: v.Text})
			case llm.ThinkingBlock:
				result.Parts = append(result.Parts, llm.ContentPart{Type: "reasoning", Text: v.Thinking})
			}
		}
	}
	calls := message.BlocksToolCalls()
	if len(calls) > 0 {
		result.Calls = make([]AgentToolCall, len(calls))
		for i, call := range calls {
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
	if len(message.Blocks) > 0 {
		return false
	}
	return strings.TrimSpace(message.TextContent()) == "" &&
		strings.TrimSpace(message.BlocksReasoning()) == "" &&
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

// parseArguments parses a JSON string into a map.
func parseArguments(args string) map[string]any {
	var m map[string]any
	if err := json.Unmarshal([]byte(args), &m); err != nil {
		return map[string]any{"raw": args}
	}
	return m
}

func usageValue(u *llm.Usage, field string) int {
	if u == nil {
		return 0
	}
	switch field {
	case "input":
		return u.InputTokens
	case "output":
		return u.OutputTokens
	case "total":
		return u.TotalTokens
	}
	return 0
}

func usageValueF(u *llm.Usage) float64 {
	if u == nil {
		return 0
	}
	return u.Cost
}

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
