package session

import (
	"context"

	"github.com/nijaru/ion/llm"
)

// userMessage creates a plain user message.
func userMessage(content string) llm.Message {
	return llm.TextMessage(llm.RoleUser, content)
}

// systemMessage creates a provider-style system message.
func systemMessage(content string) llm.Message {
	return llm.TextMessage(llm.RoleSystem, content)
}

// assistantMessage creates a plain assistant message without tool calls.
func assistantMessage(content string) llm.Message {
	return llm.TextMessage(llm.RoleAssistant, content)
}

// toolMessage creates a tool result message.
func toolMessage(name, toolID, content string) llm.Message {
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
	return NewMessage(sessionID, userMessage(content))
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
