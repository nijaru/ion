package anthropic

import (
	sdk "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/shared/constant"
	"github.com/go-json-experiment/json"
	"github.com/nijaru/ion/llm"
)

func (p *Provider) convertRequest(req *llm.Request) sdk.MessageNewParams {
	var system []sdk.TextBlockParam
	var messages []sdk.MessageParam

	for i := 0; i < len(req.Messages); i++ {
		m := req.Messages[i]
		if m.Role == llm.RoleSystem {
			block := sdk.TextBlockParam{
				Text: m.TextContent(),
				Type: constant.Text("text"),
			}
			if m.CacheControl != nil {
				block.CacheControl = sdk.NewCacheControlEphemeralParam()
			}
			system = append(system, block)
			continue
		}

		if m.Role == llm.RoleTool {
			var blocks []sdk.ContentBlockParamUnion
			for j := i; j < len(req.Messages); j++ {
				curr := req.Messages[j]
				if curr.Role != llm.RoleTool {
					i = j - 1
					break
				}
				block := p.convertToolResultBlock(curr)
				if curr.CacheControl != nil {
					block.OfToolResult.CacheControl = sdk.NewCacheControlEphemeralParam()
				}
				blocks = append(blocks, block)
				if j == len(req.Messages)-1 {
					i = j
				}
			}
			messages = append(messages, sdk.NewUserMessage(blocks...))
			continue
		}

		blocks := p.convertContentBlocks(m)
		if m.Role == llm.RoleAssistant {
			messages = append(messages, sdk.NewAssistantMessage(blocks...))
		} else {
			messages = append(messages, sdk.NewUserMessage(blocks...))
		}
	}

	params := sdk.MessageNewParams{
		Model:     sdk.Model(req.Model),
		Messages:  messages,
		Tools:     p.convertTools(req.Tools),
		MaxTokens: 4096,
	}

	if rf := req.ResponseFormat; rf != nil &&
		rf.Type == llm.ResponseFormatJSONSchema &&
		rf.Schema != nil {
		name := rf.Name
		if name == "" {
			name = "json_response"
		}
		schema := p.convertSchema(rf.Schema)
		params.Tools = append(params.Tools, sdk.ToolUnionParamOfTool(schema, name))
		params.ToolChoice = sdk.ToolChoiceParamOfTool(name)
	}

	if req.MaxTokens > 0 {
		params.MaxTokens = int64(req.MaxTokens)
	}
	if len(system) > 0 {
		params.System = system
	}
	if req.ThinkingBudget > 0 {
		params.Thinking = sdk.ThinkingConfigParamOfEnabled(int64(req.ThinkingBudget))
		params.Temperature = sdk.Float(1.0)
	} else if req.Temperature > 0 {
		params.Temperature = sdk.Float(req.Temperature)
	}

	return params
}

func (p *Provider) convertContentBlocks(m llm.Message) []sdk.ContentBlockParamUnion {
	var blocks []sdk.ContentBlockParamUnion
	for _, tb := range m.ThinkingBlocks {
		if tb.Redacted {
			blocks = append(blocks, sdk.NewRedactedThinkingBlock(tb.Signature))
		} else {
			blocks = append(blocks, sdk.NewThinkingBlock(tb.Thinking, tb.Signature))
		}
	}
	if hasImageParts(m.Parts) {
		blocks = append(blocks, p.convertContentParts(m)...)
	} else if text := m.TextContent(); text != "" {
		block := sdk.NewTextBlock(text)
		if m.CacheControl != nil {
			block.OfText.CacheControl = sdk.NewCacheControlEphemeralParam()
		}
		blocks = append(blocks, block)
	}
	calls := m.BlocksToolCalls()
	for _, call := range calls {
		block := sdk.NewToolUseBlock(call.ID, call.Function.Arguments, call.Function.Name)
		if m.CacheControl != nil {
			block.OfToolUse.CacheControl = sdk.NewCacheControlEphemeralParam()
		}
		blocks = append(blocks, block)
	}
	return blocks
}

func (p *Provider) convertContentParts(m llm.Message) []sdk.ContentBlockParamUnion {
	blocks := make([]sdk.ContentBlockParamUnion, 0, len(m.Parts))
	sawText := false
	for _, part := range m.Parts {
		switch part.Type {
		case "", llm.ContentPartText:
			if part.Text == "" {
				continue
			}
			sawText = true
			block := sdk.NewTextBlock(part.Text)
			if m.CacheControl != nil {
				block.OfText.CacheControl = sdk.NewCacheControlEphemeralParam()
			}
			blocks = append(blocks, block)
		case llm.ContentPartImage:
			block, ok := anthropicImageBlock(part)
			if !ok {
				continue
			}
			if m.CacheControl != nil {
				block.OfImage.CacheControl = sdk.NewCacheControlEphemeralParam()
			}
			blocks = append(blocks, block)
		}
	}
	if !sawText && m.Content != "" {
		block := sdk.NewTextBlock(m.Content)
		if m.CacheControl != nil {
			block.OfText.CacheControl = sdk.NewCacheControlEphemeralParam()
		}
		blocks = append([]sdk.ContentBlockParamUnion{block}, blocks...)
	}
	return blocks
}

func (p *Provider) convertToolResultBlock(m llm.Message) sdk.ContentBlockParamUnion {
	if !hasImageParts(m.Parts) {
		return sdk.NewToolResultBlock(m.ToolID, m.TextContent(), false)
	}
	content := make([]sdk.ToolResultBlockParamContentUnion, 0, len(m.Parts))
	sawText := false
	for _, part := range m.Parts {
		switch part.Type {
		case "", llm.ContentPartText:
			if part.Text == "" {
				continue
			}
			sawText = true
			content = append(content, sdk.ToolResultBlockParamContentUnion{
				OfText: &sdk.TextBlockParam{Text: part.Text},
			})
		case llm.ContentPartImage:
			image, ok := anthropicImageParam(part)
			if !ok {
				continue
			}
			content = append(content, sdk.ToolResultBlockParamContentUnion{OfImage: &image})
		}
	}
	if !sawText && m.Content != "" {
		content = append([]sdk.ToolResultBlockParamContentUnion{{
			OfText: &sdk.TextBlockParam{Text: m.Content},
		}}, content...)
	}
	if len(content) == 0 {
		return sdk.NewToolResultBlock(m.ToolID, m.TextContent(), false)
	}
	return sdk.ContentBlockParamUnion{
		OfToolResult: &sdk.ToolResultBlockParam{
			ToolUseID: m.ToolID,
			Content:   content,
			IsError:   sdk.Bool(false),
		},
	}
}

func hasImageParts(parts []llm.ContentPart) bool {
	for _, part := range parts {
		if part.Type == llm.ContentPartImage {
			return true
		}
	}
	return false
}

func anthropicImageBlock(part llm.ContentPart) (sdk.ContentBlockParamUnion, bool) {
	image, ok := anthropicImageParam(part)
	if !ok {
		return sdk.ContentBlockParamUnion{}, false
	}
	return sdk.ContentBlockParamUnion{OfImage: &image}, true
}

func anthropicImageParam(part llm.ContentPart) (sdk.ImageBlockParam, bool) {
	switch {
	case part.Data != "":
		mimeType := part.MIMEType
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		return sdk.ImageBlockParam{
			Source: sdk.ImageBlockParamSourceUnion{
				OfBase64: &sdk.Base64ImageSourceParam{
					Data:      part.Data,
					MediaType: sdk.Base64ImageSourceMediaType(mimeType),
				},
			},
		}, true
	case part.URL != "":
		return sdk.ImageBlockParam{
			Source: sdk.ImageBlockParamSourceUnion{
				OfURL: &sdk.URLImageSourceParam{URL: part.URL},
			},
		}, true
	default:
		return sdk.ImageBlockParam{}, false
	}
}

func (p *Provider) convertTools(tools []*llm.Spec) []sdk.ToolUnionParam {
	var converted []sdk.ToolUnionParam
	for _, t := range tools {
		schema := p.convertSchema(t.Parameters)
		tool := sdk.ToolUnionParamOfTool(schema, t.Name)
		if t.Description != "" {
			tool.OfTool.Description = sdk.String(t.Description)
		}
		if t.CacheControl != nil {
			tool.OfTool.CacheControl = sdk.NewCacheControlEphemeralParam()
		}
		converted = append(converted, tool)
	}
	return converted
}

// convertSchema converts a Spec.Parameters value (any JSON-serializable type)
// into the Anthropic SDK's ToolInputSchemaParam. It normalizes the input via a
// JSON round-trip so that map[string]any, json.RawMessage, typed schema structs,
// and any other serializable type are all handled uniformly.
func (p *Provider) convertSchema(params any) sdk.ToolInputSchemaParam {
	schema := sdk.ToolInputSchemaParam{
		Type: constant.Object("object"),
	}

	if params == nil {
		return schema
	}

	raw, err := json.Marshal(params)
	if err != nil {
		return schema
	}
	var m map[string]any
	if err := json.Unmarshal(raw, &m); err != nil {
		return schema
	}

	if props, ok := m["properties"]; ok {
		schema.Properties = props
	} else {
		schema.Properties = m
	}

	if req, ok := m["required"]; ok {
		if items, ok := req.([]any); ok {
			for _, item := range items {
				if s, ok := item.(string); ok {
					schema.Required = append(schema.Required, s)
				}
			}
		}
	}

	return schema
}
