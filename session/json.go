package session

import (
	"encoding/json"
	"fmt"
	"time"
)

// Custom JSON marshal/unmarshal for Message using the persisted role
// discriminator.

func (m *UserMessage) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Role      string    `json:"role"`
		Content   []Content `json:"content"`
		Timestamp int64     `json:"timestamp"`
	}{Role: "user", Content: m.Content, Timestamp: m.Timestamp.UnixMilli()})
}

func (m *AssistantMessage) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Role          string        `json:"role"`
		Content       []Content     `json:"content"`
		API           string        `json:"api,omitempty"`
		Provider      string        `json:"provider,omitempty"`
		Model         string        `json:"model,omitempty"`
		ResponseModel string        `json:"response_model,omitempty"`
		ResponseID    string        `json:"response_id,omitempty"`
		Usage         Usage         `json:"usage"`
		StopReason    StopReason    `json:"stop_reason"`
		Error         string        `json:"error,omitempty"`
		ThinkingLevel ThinkingLevel `json:"thinking_level,omitempty"`
		Timestamp     int64         `json:"timestamp"`
	}{
		Role: "assistant", Content: m.Content, API: m.API, Provider: m.Provider,
		Model: m.Model, ResponseModel: m.ResponseModel, ResponseID: m.ResponseID,
		Usage: m.Usage, StopReason: m.StopReason, Error: m.Error,
		ThinkingLevel: m.ThinkingLevel,
		Timestamp:     m.Timestamp.UnixMilli(),
	})
}

func (m *ToolResultMessage) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Role       string          `json:"role"`
		ToolCallID string          `json:"tool_call_id"`
		ToolName   string          `json:"tool_name"`
		Content    []Content       `json:"content"`
		Details    json.RawMessage `json:"details,omitempty"`
		IsError    bool            `json:"is_error"`
		Timestamp  int64           `json:"timestamp"`
	}{
		Role: "tool_result", ToolCallID: m.ToolCallID, ToolName: m.ToolName,
		Content: m.Content, Details: m.Details, IsError: m.IsError,
		Timestamp: m.Timestamp.UnixMilli(),
	})
}

// unmarshalMessage dispatches on the "role" field to the correct concrete type.
// Can't define UnmarshalJSON on an interface; callers use this instead.
func unmarshalMessage(b []byte) (Message, error) {
	var env struct {
		Role string `json:"role"`
	}
	if err := json.Unmarshal(b, &env); err != nil {
		return nil, err
	}
	switch env.Role {
	case "user":
		var raw struct {
			Content   []json.RawMessage `json:"content"`
			Timestamp int64             `json:"timestamp"`
		}
		if err := json.Unmarshal(b, &raw); err != nil {
			return nil, err
		}
		content, err := unmarshalContentSlice(raw.Content)
		if err != nil {
			return nil, err
		}
		return &UserMessage{Content: content, Timestamp: time.UnixMilli(raw.Timestamp)}, nil
	case "assistant":
		var raw struct {
			Content       []json.RawMessage `json:"content"`
			API           string            `json:"api"`
			Provider      string            `json:"provider"`
			Model         string            `json:"model"`
			ResponseModel string            `json:"response_model"`
			ResponseID    string            `json:"response_id"`
			Usage         Usage             `json:"usage"`
			StopReason    StopReason        `json:"stop_reason"`
			Error         string            `json:"error"`
			ThinkingLevel ThinkingLevel     `json:"thinking_level"`
			Timestamp     int64             `json:"timestamp"`
		}
		if err := json.Unmarshal(b, &raw); err != nil {
			return nil, err
		}
		content, err := unmarshalContentSlice(raw.Content)
		if err != nil {
			return nil, err
		}
		return &AssistantMessage{
			Content: content, API: raw.API, Provider: raw.Provider, Model: raw.Model,
			ResponseModel: raw.ResponseModel, ResponseID: raw.ResponseID,
			Usage: raw.Usage, StopReason: raw.StopReason, Error: raw.Error,
			ThinkingLevel: raw.ThinkingLevel,
			Timestamp:     time.UnixMilli(raw.Timestamp),
		}, nil
	case "tool_result":
		var raw struct {
			ToolCallID string            `json:"tool_call_id"`
			ToolName   string            `json:"tool_name"`
			Content    []json.RawMessage `json:"content"`
			Details    json.RawMessage   `json:"details"`
			IsError    bool              `json:"is_error"`
			Timestamp  int64             `json:"timestamp"`
		}
		if err := json.Unmarshal(b, &raw); err != nil {
			return nil, err
		}
		content, err := unmarshalContentSlice(raw.Content)
		if err != nil {
			return nil, err
		}
		return &ToolResultMessage{
			ToolCallID: raw.ToolCallID, ToolName: raw.ToolName,
			Content: content, Details: raw.Details, IsError: raw.IsError,
			Timestamp: time.UnixMilli(raw.Timestamp),
		}, nil
	default:
		return nil, fmt.Errorf("unknown message role %q", env.Role)
	}
}

// Content JSON: discriminated by "type" field.

func (c TextContent) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct{ Type, Text string }{Type: "text", Text: c.Text})
}

func (c ThinkingContent) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Type      string `json:"type"`
		Text      string `json:"text"`
		Signature string `json:"signature,omitempty"`
		Redacted  bool   `json:"redacted,omitempty"`
	}{Type: "thinking", Text: c.Text, Signature: c.Signature, Redacted: c.Redacted})
}

func (c ImageContent) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Type     string `json:"type"`
		Data     []byte `json:"data"`
		MimeType string `json:"mime_type"`
	}{Type: "image", Data: c.Data, MimeType: c.MimeType})
}

func (c *ToolCall) MarshalJSON() ([]byte, error) {
	return json.Marshal(struct {
		Type      string         `json:"type"`
		ID        string         `json:"id"`
		Name      string         `json:"name"`
		Arguments map[string]any `json:"arguments"`
		Signature string         `json:"signature,omitempty"`
	}{Type: "tool_call", ID: c.ID, Name: c.Name, Arguments: c.Arguments, Signature: c.Signature})
}

// unmarshalContent dispatches Content blocks on the "type" field.
func unmarshalContent(b []byte) (Content, error) {
	var env struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(b, &env); err != nil {
		return nil, err
	}
	switch env.Type {
	case "text":
		var t TextContent
		if err := json.Unmarshal(b, &t); err != nil {
			return nil, err
		}
		return t, nil
	case "thinking":
		var t ThinkingContent
		if err := json.Unmarshal(b, &t); err != nil {
			return nil, err
		}
		return t, nil
	case "image":
		var i ImageContent
		if err := json.Unmarshal(b, &i); err != nil {
			return nil, err
		}
		return i, nil
	case "tool_call":
		var tc ToolCall
		if err := json.Unmarshal(b, &tc); err != nil {
			return nil, err
		}
		return &tc, nil
	default:
		return nil, fmt.Errorf("unknown content type %q", env.Type)
	}
}

func unmarshalContentSlice(raw []json.RawMessage) ([]Content, error) {
	result := make([]Content, len(raw))
	for i, b := range raw {
		c, err := unmarshalContent(b)
		if err != nil {
			return nil, err
		}
		result[i] = c
	}
	return result, nil
}
