package openai

import (
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/ion/llm"
	"github.com/sashabaranov/go-openai"
)

// ConvertRequest transforms the unified Request into OpenAI's format.
func (b *Base) ConvertRequest(req *llm.Request) openai.ChatCompletionRequest {
	messages := make([]openai.ChatCompletionMessage, len(req.Messages))
	for i, m := range req.Messages {
		content, multiContent := b.convertMessageContent(m)
		msg := openai.ChatCompletionMessage{
			Role:         string(m.Role),
			Content:      content,
			MultiContent: multiContent,
			Name:         m.Name,
		}
		if len(m.Calls) > 0 {
			msg.ToolCalls = make([]openai.ToolCall, len(m.Calls))
			for j, call := range m.Calls {
				msg.ToolCalls[j] = openai.ToolCall{
					ID:   call.ID,
					Type: openai.ToolType(call.Type),
					Function: openai.FunctionCall{
						Name:      call.Function.Name,
						Arguments: call.Function.Arguments,
					},
				}
			}
		}
		if m.Role == llm.RoleTool {
			msg.ToolCallID = m.ToolID
		}
		messages[i] = msg
	}

	var tools []openai.Tool
	if len(req.Tools) > 0 {
		tools = make([]openai.Tool, len(req.Tools))
		for i, t := range req.Tools {
			tools[i] = openai.Tool{
				Type: openai.ToolTypeFunction,
				Function: &openai.FunctionDefinition{
					Name:        t.Name,
					Description: t.Description,
					Parameters:  t.Parameters,
				},
			}
		}
	}

	caps := b.Capabilities(req.Model)
	compat := b.CompatSettings()
	cr := openai.ChatCompletionRequest{
		Model:         req.Model,
		Messages:      messages,
		Tools:         tools,
		StreamOptions: &openai.StreamOptions{IncludeUsage: true},
	}

	// Use ProviderCompat to determine max tokens field
	if caps.Temperature {
		cr.Temperature = float32(req.Temperature)
		if compat.MaxTokensField == "max_tokens" {
			cr.MaxTokens = req.MaxTokens
		} else {
			cr.MaxCompletionTokens = req.MaxTokens
		}
	} else {
		// Models without temperature control require max_completion_tokens,
		// which counts both visible output and internal reasoning tokens.
		if compat.MaxTokensField == "max_tokens" {
			cr.MaxTokens = req.MaxTokens
		} else {
			cr.MaxCompletionTokens = req.MaxTokens
		}
	}

	// Use ProviderCompat ThinkingFormat to determine reasoning format
	b.applyReasoningFormat(&cr, req, caps, compat)

	if rf := req.ResponseFormat; rf != nil {
		switch rf.Type {
		case llm.ResponseFormatJSON:
			cr.ResponseFormat = &openai.ChatCompletionResponseFormat{
				Type: openai.ChatCompletionResponseFormatTypeJSONObject,
			}
		case llm.ResponseFormatJSONSchema:
			cr.ResponseFormat = &openai.ChatCompletionResponseFormat{
				Type: openai.ChatCompletionResponseFormatTypeJSONSchema,
				JSONSchema: &openai.ChatCompletionResponseFormatJSONSchema{
					Name:   rf.Name,
					Schema: schemaMarshaler(rf.Schema),
					Strict: rf.Strict,
				},
			}
		}
	}
	return cr
}

// applyReasoningFormat applies the appropriate reasoning format based on ProviderCompat.
func (b *Base) applyReasoningFormat(
	cr *openai.ChatCompletionRequest,
	req *llm.Request,
	caps llm.Capabilities,
	compat llm.ProviderCompat,
) {
	switch compat.ThinkingFormat {
	case llm.ThinkingFormatZai, llm.ThinkingFormatQwen:
		if caps.ReasoningCaps().Kind == llm.ReasoningKindBoolean {
			cr.ChatTemplateKwargs = map[string]any{
				"enable_thinking":   reasoningToggleEnabled(req.ReasoningEffort),
				"preserve_thinking": true,
			}
		}

	case llm.ThinkingFormatQwenChatTemplate:
		if caps.ReasoningCaps().Kind == llm.ReasoningKindBoolean {
			cr.ChatTemplateKwargs = map[string]any{
				"enable_thinking":   reasoningToggleEnabled(req.ReasoningEffort),
				"preserve_thinking": true,
			}
		}

	case llm.ThinkingFormatDeepSeek:
		if caps.ReasoningCaps().Kind == llm.ReasoningKindEffort {
			cr.ChatTemplateKwargs = map[string]any{
				"thinking": map[string]any{
					"type": reasoningToggleEnabled(req.ReasoningEffort),
				},
			}
			if req.ReasoningEffort != "" && reasoningToggleEnabled(req.ReasoningEffort) {
				cr.ReasoningEffort = req.ReasoningEffort
			}
		}

	case llm.ThinkingFormatOpenRouter:
		// OpenRouter uses nested reasoning object - handled by the OpenRouter provider
		// For now, we just pass through the reasoning effort
		if caps.SupportsReasoningEffort(req.ReasoningEffort) {
			cr.ReasoningEffort = req.ReasoningEffort
		}

	case llm.ThinkingFormatTogether:
		// Together uses reasoning: { enabled: bool } plus reasoning_effort
		if caps.ReasoningCaps().Kind == llm.ReasoningKindEffort {
			cr.ChatTemplateKwargs = map[string]any{
				"reasoning": map[string]any{
					"enabled": reasoningToggleEnabled(req.ReasoningEffort),
				},
			}
			if req.ReasoningEffort != "" && reasoningToggleEnabled(req.ReasoningEffort) && compat.SupportsReasoningEffort {
				cr.ReasoningEffort = req.ReasoningEffort
			}
		}

	default: // ThinkingFormatOpenAI
		if caps.ReasoningCaps().Kind == llm.ReasoningKindBoolean &&
			caps.SupportsReasoningToggle(req.ReasoningEffort) {
			cr.ChatTemplateKwargs = map[string]any{
				"enable_thinking":   reasoningToggleEnabled(req.ReasoningEffort),
				"preserve_thinking": true,
			}
		} else if caps.SupportsReasoningEffort(req.ReasoningEffort) {
			cr.ReasoningEffort = req.ReasoningEffort
		}
	}
}

func (b *Base) convertMessageContent(m llm.Message) (string, []openai.ChatMessagePart) {
	if !hasImageParts(m.Parts) {
		return m.TextContent(), nil
	}

	parts := make([]openai.ChatMessagePart, 0, len(m.Parts))
	sawText := false
	for _, part := range m.Parts {
		switch part.Type {
		case "", llm.ContentPartText:
			if part.Text == "" {
				continue
			}
			sawText = true
			parts = append(parts, openai.ChatMessagePart{
				Type: openai.ChatMessagePartTypeText,
				Text: part.Text,
			})
		case llm.ContentPartImage:
			imageURL := imagePartURL(part)
			if imageURL == "" {
				continue
			}
			parts = append(parts, openai.ChatMessagePart{
				Type: openai.ChatMessagePartTypeImageURL,
				ImageURL: &openai.ChatMessageImageURL{
					URL:    imageURL,
					Detail: openai.ImageURLDetailAuto,
				},
			})
		}
	}
	if !sawText && m.Content != "" {
		parts = append([]openai.ChatMessagePart{{
			Type: openai.ChatMessagePartTypeText,
			Text: m.Content,
		}}, parts...)
	}
	if len(parts) == 0 {
		return m.TextContent(), nil
	}
	return "", parts
}

func hasImageParts(parts []llm.ContentPart) bool {
	for _, part := range parts {
		if part.Type == llm.ContentPartImage {
			return true
		}
	}
	return false
}

func imagePartURL(part llm.ContentPart) string {
	if part.URL != "" {
		return part.URL
	}
	if part.Data == "" {
		return ""
	}
	mimeType := part.MIMEType
	if mimeType == "" {
		mimeType = "application/octet-stream"
	}
	return "data:" + mimeType + ";base64," + part.Data
}

func reasoningToggleEnabled(value string) bool {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "off", "none", "disabled":
		return false
	default:
		return true
	}
}

// schemaMarshaler wraps a map[string]any to implement json.Marshaler,
// as required by the OpenAI SDK's JSONSchema field.
type schemaMarshaler map[string]any

func (s schemaMarshaler) MarshalJSON() ([]byte, error) {
	return json.Marshal(map[string]any(s))
}

// buildBlocks constructs ContentBlocks from flat OpenAI message fields.
func buildBlocks(content string, reasoning string, toolCalls []openai.ToolCall) []llm.ContentBlock {
	var blocks []llm.ContentBlock
	if reasoning != "" {
		blocks = append(blocks, llm.ThinkingBlock{Thinking: reasoning})
	}
	if content != "" {
		blocks = append(blocks, llm.TextBlock{Text: content})
	}
	for _, tc := range toolCalls {
		blocks = append(blocks, llm.ToolCallBlock{
			ID:        tc.ID,
			Name:      tc.Function.Name,
			Arguments: tc.Function.Arguments,
		})
	}
	if len(blocks) == 0 {
		return nil
	}
	return blocks
}

// ConvertToolCalls transforms OpenAI tool calls into the unified format.
func (b *Base) ConvertToolCalls(calls []openai.ToolCall) []llm.Call {
	if len(calls) == 0 {
		return nil
	}
	res := make([]llm.Call, len(calls))
	for i, call := range calls {
		res[i] = llm.Call{
			ID:   call.ID,
			Type: string(call.Type),
		}
		res[i].Function.Name = call.Function.Name
		res[i].Function.Arguments = call.Function.Arguments
	}
	return res
}
