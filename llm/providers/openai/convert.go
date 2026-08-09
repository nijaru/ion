package openai

import (
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/ion/llm"
	"github.com/sashabaranov/go-openai"
)

// ConvertRequest transforms the unified Request into a non-streaming OpenAI
// request. Stream-only options are added by ConvertStreamingRequest.
func (b *Base) ConvertRequest(req *llm.Request) openai.ChatCompletionRequest {
	return b.convertRequest(req, false)
}

// ConvertStreamingRequest transforms the unified Request into an OpenAI
// streaming request, including usage options when the model supports them.
func (b *Base) ConvertStreamingRequest(req *llm.Request) openai.ChatCompletionRequest {
	return b.convertRequest(req, true)
}

func (b *Base) convertRequest(req *llm.Request, streaming bool) openai.ChatCompletionRequest {
	compat := b.CompatSettingsForModel(req.Model)
	caps := llm.CapabilitiesForRequest(req, b.Capabilities(req.Model))
	messages := make([]openai.ChatCompletionMessage, 0, len(req.Messages))
	lastRole := ""
	for i := 0; i < len(req.Messages); i++ {
		m := req.Messages[i]
		if m.Role == llm.RoleTool {
			var imageParts []openai.ChatMessagePart
			for ; i < len(req.Messages) && req.Messages[i].Role == llm.RoleTool; i++ {
				tool := req.Messages[i]
				content := tool.TextContent()
				if content == "" {
					if hasImageParts(tool.Parts) {
						content = "(see attached image)"
					} else {
						content = "(no tool output)"
					}
				}
				msg := openai.ChatCompletionMessage{
					Role:       string(llm.RoleTool),
					Content:    content,
					ToolCallID: normalizeOpenAIToolCallID(tool.ToolID),
				}
				if compat.RequiresToolResultName {
					msg.Name = tool.Name
				}
				messages = append(messages, msg)
				if caps.SupportsImages() {
					for _, part := range tool.Parts {
						if part.Type != llm.ContentPartImage {
							continue
						}
						if imageURL := imagePartURL(part); imageURL != "" {
							imageParts = append(imageParts, openai.ChatMessagePart{
								Type: openai.ChatMessagePartTypeImageURL,
								ImageURL: &openai.ChatMessageImageURL{
									URL:    imageURL,
									Detail: openai.ImageURLDetailAuto,
								},
							})
						}
					}
				}
			}
			i--
			if len(imageParts) > 0 {
				if compat.RequiresAssistantAfterToolResult {
					messages = append(messages, openai.ChatCompletionMessage{
						Role:    string(llm.RoleAssistant),
						Content: "I have processed the tool results.",
					})
				}
				parts := make([]openai.ChatMessagePart, 0, len(imageParts)+1)
				parts = append(parts, openai.ChatMessagePart{
					Type: openai.ChatMessagePartTypeText,
					Text: "Attached image(s) from tool result:",
				})
				parts = append(parts, imageParts...)
				messages = append(messages, openai.ChatCompletionMessage{
					Role:         string(llm.RoleUser),
					MultiContent: parts,
				})
				lastRole = string(llm.RoleUser)
			} else {
				lastRole = string(llm.RoleTool)
			}
			continue
		}
		if compat.RequiresAssistantAfterToolResult && lastRole == string(llm.RoleTool) && m.Role == llm.RoleUser {
			messages = append(messages, openai.ChatCompletionMessage{
				Role:    string(llm.RoleAssistant),
				Content: "I have processed the tool results.",
			})
		}

		content, multiContent := b.convertMessageContent(m)
		role := string(m.Role)
		if m.Role == llm.RoleDeveloper && !compat.SupportsDeveloperRole {
			role = string(llm.RoleSystem)
		}
		msg := openai.ChatCompletionMessage{
			Role:         role,
			Content:      content,
			MultiContent: multiContent,
			Name:         m.Name,
		}
		if m.Role == llm.RoleAssistant {
			reasoning := m.BlocksReasoning()
			if compat.RequiresReasoningContentOnAssistantMessages && reasoning != "" {
				// The upstream SDK omits empty reasoning_content on the wire;
				// preserve meaningful replayed reasoning without fabricating a
				// sentinel value for ordinary assistant messages.
				msg.ReasoningContent = reasoning
			}
			if compat.RequiresThinkingAsText && reasoning != "" {
				thinkingText := thinkingAsText(reasoning)
				if len(msg.MultiContent) == 0 {
					msg.Content = appendThinkingText(msg.Content, thinkingText)
				} else {
					msg.MultiContent = append([]openai.ChatMessagePart{{
						Type: openai.ChatMessagePartTypeText,
						Text: thinkingText,
					}}, msg.MultiContent...)
				}
			}
		}
		calls := m.BlocksToolCalls()
		if len(calls) > 0 {
			msg.ToolCalls = make([]openai.ToolCall, len(calls))
			for j, call := range calls {
				msg.ToolCalls[j] = openai.ToolCall{
					ID:   normalizeOpenAIToolCallID(call.ID),
					Type: openai.ToolType(call.Type),
					Function: openai.FunctionCall{
						Name:      call.Function.Name,
						Arguments: call.Function.Arguments,
					},
				}
			}
		}
		messages = append(messages, msg)
		lastRole = role
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

	cr := openai.ChatCompletionRequest{
		Model:    req.Model,
		Messages: messages,
		Tools:    tools,
	}
	if streaming && compat.SupportsStreamOptions {
		cr.StreamOptions = &openai.StreamOptions{IncludeUsage: true}
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
	effort := b.ReasoningEffortForModel(req.Model, req.ReasoningEffort)
	supportsEffort := caps.SupportsReasoningEffort(req.ReasoningEffort) || caps.SupportsReasoningEffort(effort)
	supportsToggle := caps.SupportsReasoningToggle(req.ReasoningEffort) || caps.SupportsReasoningToggle(effort)
	switch compat.ThinkingFormat {
	case llm.ThinkingFormatZai, llm.ThinkingFormatQwen:
		if caps.ReasoningCaps().Kind == llm.ReasoningKindBoolean {
			cr.ChatTemplateKwargs = map[string]any{
				"enable_thinking":   reasoningToggleEnabled(effort),
				"preserve_thinking": true,
			}
		}

	case llm.ThinkingFormatQwenChatTemplate:
		if caps.ReasoningCaps().Kind == llm.ReasoningKindBoolean {
			cr.ChatTemplateKwargs = map[string]any{
				"enable_thinking":   reasoningToggleEnabled(effort),
				"preserve_thinking": true,
			}
		}

	case llm.ThinkingFormatDeepSeek:
		if caps.ReasoningCaps().Kind == llm.ReasoningKindEffort {
			cr.ChatTemplateKwargs = map[string]any{
				"thinking": map[string]any{
					"type": reasoningToggleEnabled(effort),
				},
			}
			if effort != "" && reasoningToggleEnabled(effort) {
				cr.ReasoningEffort = effort
			}
		}

	case llm.ThinkingFormatOpenRouter:
		// OpenRouter uses nested reasoning object - handled by the OpenRouter provider
		// For now, we just pass through the reasoning effort
		if supportsEffort {
			cr.ReasoningEffort = effort
		}

	case llm.ThinkingFormatTogether:
		// Together uses reasoning: { enabled: bool } plus reasoning_effort
		if caps.ReasoningCaps().Kind == llm.ReasoningKindEffort {
			cr.ChatTemplateKwargs = map[string]any{
				"reasoning": map[string]any{
					"enabled": reasoningToggleEnabled(effort),
				},
			}
			if effort != "" && reasoningToggleEnabled(effort) && compat.SupportsReasoningEffort {
				cr.ReasoningEffort = effort
			}
		}

	default: // ThinkingFormatOpenAI
		if caps.ReasoningCaps().Kind == llm.ReasoningKindBoolean && supportsToggle {
			cr.ChatTemplateKwargs = map[string]any{
				"enable_thinking":   reasoningToggleEnabled(effort),
				"preserve_thinking": true,
			}
		} else if supportsEffort {
			cr.ReasoningEffort = effort
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
	if !sawText {
		if text := m.TextContent(); text != "" {
			parts = append([]openai.ChatMessagePart{{
				Type: openai.ChatMessagePartTypeText,
				Text: text,
			}}, parts...)
		}
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

func normalizeOpenAIToolCallID(id string) string {
	if len(id) <= 40 {
		return id
	}
	return id[:40]
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

func thinkingAsText(reasoning string) string {
	// Pi's OpenAI-compatible adapter replays thinking as plain text for
	// providers that do not accept a native reasoning field. Tags become
	// prompt content and can encourage a model to reproduce them, so preserve
	// only the reasoning itself at this wire boundary.
	return reasoning
}

func appendThinkingText(content, thinking string) string {
	if content == "" {
		return thinking
	}
	return thinking + "\n\n" + content
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
