package openaicodex

import (
	"encoding/json"
	"strings"

	"github.com/nijaru/ion/llm"
)

type requestBody struct {
	Model             string             `json:"model"`
	Store             bool               `json:"store"`
	Stream            bool               `json:"stream"`
	Instructions      string             `json:"instructions"`
	Input             []any              `json:"input"`
	Text              responseText       `json:"text"`
	Include           []string           `json:"include,omitempty"`
	Reasoning         *responseReasoning `json:"reasoning,omitempty"`
	Tools             []responseTool     `json:"tools,omitempty"`
	ToolChoice        string             `json:"tool_choice,omitempty"`
	ParallelToolCalls bool               `json:"parallel_tool_calls"`
	MaxOutputTokens   int                `json:"max_output_tokens,omitempty"`
	PromptCacheKey    string             `json:"prompt_cache_key,omitempty"`
}

type responseText struct {
	Verbosity string `json:"verbosity"`
}

type responseReasoning struct {
	Effort  string `json:"effort"`
	Summary string `json:"summary"`
}

type responseTool struct {
	Type        string `json:"type"`
	Name        string `json:"name"`
	Description string `json:"description,omitempty"`
	Parameters  any    `json:"parameters"`
	Strict      bool   `json:"strict"`
}

type responseMessage struct {
	Role    string         `json:"role"`
	Content []responsePart `json:"content"`
}

type responsePart struct {
	Type     string `json:"type"`
	Text     string `json:"text,omitempty"`
	ImageURL string `json:"image_url,omitempty"`
}

type responseFunctionCall struct {
	Type      string `json:"type"`
	ID        string `json:"id,omitempty"`
	CallID    string `json:"call_id"`
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
}

type responseFunctionOutput struct {
	Type   string `json:"type"`
	CallID string `json:"call_id"`
	Output string `json:"output"`
}

func buildRequest(req *llm.Request) requestBody {
	body := requestBody{
		Model:             req.Model,
		Store:             false,
		Stream:            true,
		Text:              responseText{Verbosity: "low"},
		Include:           []string{"reasoning.encrypted_content"},
		ToolChoice:        "auto",
		ParallelToolCalls: true,
		MaxOutputTokens:   req.MaxTokens,
		PromptCacheKey:    req.SessionID,
	}
	var instructions []string
	for _, message := range req.Messages {
		switch message.Role {
		case llm.RoleSystem, llm.RoleDeveloper:
			if text := strings.TrimSpace(message.TextContent()); text != "" {
				instructions = append(instructions, text)
			}
		case llm.RoleUser:
			body.Input = append(body.Input, responseMessage{
				Role:    "user",
				Content: responseParts(message),
			})
		case llm.RoleAssistant:
			appendAssistantInput(&body, message)
		case llm.RoleTool:
			callID, _ := splitCodexCallID(message.ToolID)
			body.Input = append(body.Input, responseFunctionOutput{
				Type: "function_call_output", CallID: callID, Output: message.TextContent(),
			})
		}
	}
	body.Instructions = strings.Join(instructions, "\n\n")
	if body.Instructions == "" {
		body.Instructions = "You are a helpful assistant."
	}
	if effort := strings.ToLower(strings.TrimSpace(req.ReasoningEffort)); effort != "" && effort != "auto" && effort != "off" {
		if effort == "minimal" {
			effort = "low"
		}
		body.Reasoning = &responseReasoning{Effort: effort, Summary: "auto"}
	}
	for _, tool := range req.Tools {
		if tool == nil {
			continue
		}
		body.Tools = append(body.Tools, responseTool{
			Type: "function", Name: tool.Name, Description: tool.Description, Parameters: tool.Parameters,
		})
	}
	return body
}

func appendAssistantInput(body *requestBody, message llm.Message) {
	for _, block := range message.GetContentBlocks() {
		switch value := block.(type) {
		case llm.TextBlock:
			if value.Text != "" {
				body.Input = append(body.Input, responseMessage{
					Role:    "assistant",
					Content: []responsePart{{Type: "output_text", Text: value.Text}},
				})
			}
		case llm.ThinkingBlock:
			if value.Signature == "" {
				continue
			}
			var item any
			if err := json.Unmarshal([]byte(value.Signature), &item); err == nil {
				body.Input = append(body.Input, item)
			}
		case llm.ToolCallBlock:
			callID, itemID := splitCodexCallID(value.ID)
			body.Input = append(body.Input, responseFunctionCall{
				Type: "function_call", ID: itemID, CallID: callID, Name: value.Name, Arguments: value.Arguments,
			})
		}
	}
}

func splitCodexCallID(value string) (callID, itemID string) {
	callID, itemID = value, ""
	if left, right, ok := strings.Cut(value, "|"); ok {
		callID, itemID = left, right
	}
	return callID, itemID
}

func responseParts(message llm.Message) []responsePart {
	if len(message.Parts) == 0 {
		return []responsePart{{Type: "input_text", Text: message.TextContent()}}
	}
	parts := make([]responsePart, 0, len(message.Parts))
	for _, part := range message.Parts {
		switch part.Type {
		case llm.ContentPartImage:
			imageURL := part.URL
			if imageURL == "" && part.Data != "" {
				imageURL = "data:" + part.MIMEType + ";base64," + part.Data
			}
			parts = append(parts, responsePart{Type: "input_image", ImageURL: imageURL})
		default:
			parts = append(parts, responsePart{Type: "input_text", Text: part.Text})
		}
	}
	return parts
}
