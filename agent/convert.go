package agent

import (
	"encoding/base64"
	"encoding/json"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// DefaultConvert transforms domain Messages to provider Messages at the boundary.
// This is the default for LoopConfig.Convert when nil.
// Filters to user/assistant/tool_result roles.
func DefaultConvert(msgs []session.Message) []llm.Message {
	result := make([]llm.Message, 0, len(msgs))
	for _, m := range msgs {
		switch m := m.(type) {
		case *session.UserMessage:
			result = append(result, convertUser(m))
		case *session.AssistantMessage:
			// Failed assistant records remain useful session evidence, but an
			// incomplete error/abort response is not a valid provider transcript
			// item to replay. Partial text or thinking can still be incomplete,
			// and providers may reject or misinterpret it on the next turn.
			if m.StopReason == session.StopReasonError || m.StopReason == session.StopReasonAborted {
				continue
			}
			converted := convertAssistant(m)
			if !converted.HasAssistantPayload() {
				continue
			}
			result = append(result, converted)
		case *session.ToolResultMessage:
			result = append(result, convertToolResult(m))
		case *session.CustomMessage:
			result = append(result, convertCustomMessage(m))
		}
	}
	return result
}

func convertUser(m *session.UserMessage) llm.Message {
	msg := llm.Message{Role: llm.RoleUser, Timestamp: m.Timestamp.UnixMilli()}
	for _, c := range m.Content {
		switch c := c.(type) {
		case session.TextContent:
			msg.Content += c.Text
			msg.Parts = append(msg.Parts, llm.TextPart(c.Text))
		case session.ImageContent:
			msg.Parts = append(msg.Parts, llm.ContentPart{
				Type:     llm.ContentPartImage,
				MIMEType: c.MimeType,
				Data:     base64.StdEncoding.EncodeToString(c.Data),
			})
		}
	}
	return msg
}

func convertAssistant(m *session.AssistantMessage) llm.Message {
	msg := llm.Message{
		Role:          llm.RoleAssistant,
		API:           m.API,
		Provider:      m.Provider,
		Model:         m.Model,
		ResponseModel: m.ResponseModel,
		ResponseID:    m.ResponseID,
		StopReason:    llm.StopReason(m.StopReason),
		ErrorMessage:  m.Error,
		Timestamp:     m.Timestamp.UnixMilli(),
	}
	for _, c := range m.Content {
		switch c := c.(type) {
		case session.TextContent:
			msg.Content += c.Text
			msg.Parts = append(msg.Parts, llm.TextPart(c.Text))
		case session.ThinkingContent:
			msg.Reasoning += c.Text
			msg.ThinkingBlocks = append(msg.ThinkingBlocks, llm.ThinkingBlock{
				Thinking: c.Text, Signature: c.Signature, Redacted: c.Redacted,
			})
		case *session.ToolCall:
			args, _ := json.Marshal(c.Arguments)
			msg.Calls = append(msg.Calls, llm.Call{
				ID:   c.ID,
				Type: "function", // LLM API type, not the session's internal "tool_call" discriminator
				Function: struct {
					Name      string `json:"name"`
					Arguments string `json:"arguments"`
				}{Name: c.Name, Arguments: string(args)},
			})
		}
	}
	return msg
}

func convertToolResult(m *session.ToolResultMessage) llm.Message {
	msg := llm.Message{
		Role:    llm.RoleTool,
		ToolID:  m.ToolCallID,
		Name:    m.ToolName,
		IsError: m.IsError,
	}
	for _, c := range m.Content {
		switch c := c.(type) {
		case session.TextContent:
			msg.Content += c.Text
			msg.Parts = append(msg.Parts, llm.TextPart(c.Text))
		case session.ImageContent:
			msg.Parts = append(msg.Parts, llm.ContentPart{
				Type:     llm.ContentPartImage,
				MIMEType: c.MimeType,
				Data:     base64.StdEncoding.EncodeToString(c.Data),
			})
		}
	}
	return msg
}

func convertCustomMessage(m *session.CustomMessage) llm.Message {
	msg := llm.Message{
		Role: llm.RoleUser,
	}
	for _, c := range m.Content {
		switch c := c.(type) {
		case session.TextContent:
			msg.Content += c.Text
			msg.Parts = append(msg.Parts, llm.TextPart(c.Text))
		case session.ImageContent:
			msg.Parts = append(msg.Parts, llm.ContentPart{
				Type:     llm.ContentPartImage,
				MIMEType: c.MimeType,
				Data:     base64.StdEncoding.EncodeToString(c.Data),
			})
		}
	}
	return msg
}
