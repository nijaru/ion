package session

import (
	"context"

	"github.com/nijaru/ion/internal/llm"
)

// UserMessage creates a plain user message.
func UserMessage(content string) llm.Message {
	return llm.TextMessage(llm.RoleUser, content)
}

// SystemMessage creates a provider-style system message. Durable session
// context should use ContextEntry via NewContext or AppendContext instead.
func SystemMessage(content string) llm.Message {
	return llm.TextMessage(llm.RoleSystem, content)
}

// AssistantMessage creates a plain assistant message without tool calls.
func AssistantMessage(content string) llm.Message {
	return llm.TextMessage(llm.RoleAssistant, content)
}

// ToolMessage creates a tool result message.
func ToolMessage(name, toolID, content string) llm.Message {
	msg := llm.TextMessage(llm.RoleTool, content)
	msg.Name = name
	msg.ToolID = toolID
	return msg
}

// NewPromptMessages creates message-added events for typed prompt input.
func NewPromptMessages(sessionID string, prompt llm.Prompt) []Event {
	messages := prompt.Clone().Messages
	events := make([]Event, 0, len(messages))
	for _, msg := range messages {
		events = append(events, NewMessage(sessionID, msg))
	}
	return events
}

// NewUserMessage creates a message-added event for a plain user message.
func NewUserMessage(sessionID string, content string) Event {
	return NewMessage(sessionID, UserMessage(content))
}

// AppendUser appends a plain user message to the session.
func (s *Session) AppendUser(ctx context.Context, content string) error {
	return s.Append(ctx, NewUserMessage(s.ID(), content))
}

// AppendPrompt appends typed prompt messages to the session.
func (s *Session) AppendPrompt(ctx context.Context, prompt llm.Prompt) error {
	for _, event := range NewPromptMessages(s.ID(), prompt) {
		if err := s.Append(ctx, event); err != nil {
			return err
		}
	}
	return nil
}
