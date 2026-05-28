package llm

import "strings"

// Role defines the role of a message in the conversation.
type Role string

const (
	RoleSystem    Role = "system"
	RoleUser      Role = "user"
	RoleAssistant Role = "assistant"
	RoleTool      Role = "tool"
	// RoleDeveloper is a privileged instruction channel accepted by some models.
	// Capabilities converts system messages to this role when
	// Capabilities.SystemRole is RoleDeveloper.
	RoleDeveloper Role = "developer"
)

// CacheControl defines the caching behavior for a block of content.
type CacheControl struct {
	Type string `json:"type"` // e.g. "ephemeral"
}

// ThinkingBlock represents a reasoning block from a provider like Anthropic.
type ThinkingBlock struct {
	Type      string `json:"type"` // "thinking" or "redacted_thinking"
	Thinking  string `json:"thinking,omitzero"`
	Signature string `json:"signature,omitzero"`
}

// ContentPartType identifies one typed part of a model-visible message.
type ContentPartType string

const (
	ContentPartText ContentPartType = "text"
	// ContentPartImage represents image input encoded as base64 data or a
	// provider-readable URL. Providers that do not support image parts should
	// fall back to the surrounding text content.
	ContentPartImage ContentPartType = "image"
)

// ContentPart represents structured model-visible message content.
type ContentPart struct {
	Type     ContentPartType `json:"type"`
	Text     string          `json:"text,omitzero"`
	MIMEType string          `json:"mime_type,omitzero"`
	Data     string          `json:"data,omitzero"`
	URL      string          `json:"url,omitzero"`
}

// TextPart creates a text content part.
func TextPart(text string) ContentPart {
	return ContentPart{Type: ContentPartText, Text: text}
}

// ImagePart creates an image content part backed by base64-encoded data.
func ImagePart(mimeType, data string) ContentPart {
	return ContentPart{Type: ContentPartImage, MIMEType: mimeType, Data: data}
}

// ImageURLPart creates an image content part backed by a provider-readable URL.
func ImageURLPart(mimeType, url string) ContentPart {
	return ContentPart{Type: ContentPartImage, MIMEType: mimeType, URL: url}
}

// Message represents a single message in the LLM conversation.
type Message struct {
	Role           Role            `json:"role"`
	Content        string          `json:"content"`
	Parts          []ContentPart   `json:"parts,omitzero"`
	Reasoning      string          `json:"reasoning,omitzero"`
	ThinkingBlocks []ThinkingBlock `json:"thinking_blocks,omitzero"`
	Name           string          `json:"name,omitzero"` // For tool output or identifying the assistant
	ToolID         string          `json:"tool_id,omitzero"`
	Calls          []Call          `json:"tool_calls,omitzero"`
	CacheControl   *CacheControl   `json:"cache_control,omitzero"`
}

// TextMessage creates a message whose text is also represented as a structured
// content part.
func TextMessage(role Role, text string) Message {
	return Message{
		Role:    role,
		Content: text,
		Parts:   []ContentPart{TextPart(text)},
	}
}

// TextContent returns provider-visible text for adapters that do not yet expose
// native content-part support.
func (m Message) TextContent() string {
	if m.Content != "" {
		return m.Content
	}
	var sb strings.Builder
	for _, part := range m.Parts {
		if part.Type == "" || part.Type == ContentPartText {
			sb.WriteString(part.Text)
		}
	}
	return sb.String()
}

// HasTextContent reports whether the message has non-empty visible text.
func (m Message) HasTextContent() bool {
	return strings.TrimSpace(m.TextContent()) != ""
}

// HasAssistantPayload reports whether an assistant message carries useful
// model-visible payload.
func (m Message) HasAssistantPayload() bool {
	return m.HasTextContent() ||
		strings.TrimSpace(m.Reasoning) != "" ||
		len(m.ThinkingBlocks) > 0 ||
		len(m.Calls) > 0
}

// Prompt is typed host input for one model turn.
type Prompt struct {
	Messages []Message `json:"messages"`
}

// TextPrompt creates a one-message user prompt.
func TextPrompt(text string) Prompt {
	return Prompt{Messages: []Message{TextMessage(RoleUser, text)}}
}

// NewPrompt creates typed turn input from one or more messages.
func NewPrompt(messages ...Message) Prompt {
	return Prompt{Messages: cloneMessages(messages)}
}

// Clone returns a deep copy of the prompt.
func (p Prompt) Clone() Prompt {
	return Prompt{Messages: cloneMessages(p.Messages)}
}

// Call represents a request from the LLM to call a tool.
type Call struct {
	ID       string `json:"id"`
	Type     string `json:"type"` // e.g., "function"
	Function struct {
		Name      string `json:"name"`
		Arguments string `json:"arguments"` // JSON string
	} `json:"function"`
}

// Spec represents a tool that can be called by the LLM.
type Spec struct {
	Name         string        `json:"name"`
	Description  string        `json:"description"`
	Parameters   any           `json:"parameters"` // JSON Schema
	CacheControl *CacheControl `json:"cache_control,omitzero"`
}

// ResponseFormatType controls how the model formats its output.
type ResponseFormatType string

const (
	// ResponseFormatText is the default unstructured text output.
	ResponseFormatText ResponseFormatType = "text"
	// ResponseFormatJSON constrains output to valid JSON (no schema enforced).
	ResponseFormatJSON ResponseFormatType = "json_object"
	// ResponseFormatJSONSchema constrains output to JSON matching a schema.
	ResponseFormatJSONSchema ResponseFormatType = "json_schema"
)

// ResponseFormat constrains LLM output to structured JSON.
// Providers that do not support structured outputs ignore this field.
type ResponseFormat struct {
	Type ResponseFormatType `json:"type"`
	// Schema is the JSON Schema definition used when Type is ResponseFormatJSONSchema.
	Schema map[string]any `json:"schema,omitzero"`
	// Name identifies the schema for providers that require a name.
	Name   string `json:"name,omitzero"`
	Strict bool   `json:"strict,omitzero"`
}

// Request is the unified request sent to any provider.
type Request struct {
	Model          string          `json:"model"`
	Messages       []Message       `json:"messages"`
	Tools          []*Spec         `json:"tools,omitzero"`
	Temperature    float64         `json:"temperature"`
	MaxTokens      int             `json:"max_tokens,omitzero"`
	ResponseFormat *ResponseFormat `json:"response_format,omitzero"`
	// CachePrefixMessages is the number of leading messages Canto expects to
	// stay stable across ordinary turn growth. Use Request's message insertion
	// methods when changing Messages so this boundary stays aligned. Provider
	// adapters ignore it; prompt cache helpers use it to place provider-neutral
	// cache markers.
	CachePrefixMessages int `json:"-"`
	// ReasoningEffort controls the depth of internal reasoning for OpenAI o-series
	// models. Accepted values: "low", "medium", "high". Empty means provider default.
	ReasoningEffort string `json:"reasoning_effort,omitzero"`
	// ThinkingBudget, when > 0, enables Anthropic extended thinking with the given
	// token budget (minimum 1024, must be less than MaxTokens).
	ThinkingBudget int `json:"thinking_budget,omitzero"`
}

// Response is the unified response from any provider.
type Response struct {
	Content        string          `json:"content"`
	Reasoning      string          `json:"reasoning,omitzero"`
	ThinkingBlocks []ThinkingBlock `json:"thinking_blocks,omitzero"`
	Calls          []Call          `json:"tool_calls,omitzero"`
	Usage          Usage           `json:"usage"`
}

// Usage tracks token consumption and cost.
type Usage struct {
	InputTokens         int     `json:"input_tokens"`
	OutputTokens        int     `json:"output_tokens"`
	CacheReadTokens     int     `json:"cache_read_tokens,omitempty"`
	CacheCreationTokens int     `json:"cache_creation_tokens,omitempty"`
	TotalTokens         int     `json:"total_tokens"`
	Cost                float64 `json:"cost,omitzero"` // USD
}

// Model describes an LLM model exposed by a provider.
type Model struct {
	ID            string        `json:"id"                       toml:"id"`
	ContextWindow int           `json:"context_window,omitzero"  toml:"context_window,omitzero"`
	CostPer1MIn   float64       `json:"cost_per_1m_in,omitzero"  toml:"cost_per_1m_in,omitzero"`
	CostPer1MOut  float64       `json:"cost_per_1m_out,omitzero" toml:"cost_per_1m_out,omitzero"`
	Capabilities  *Capabilities `json:"capabilities,omitzero"    toml:"capabilities,omitzero"`
}

// ProviderConfig captures the shared endpoint/auth/model metadata used by
// Canto's built-in provider adapters.
type ProviderConfig struct {
	ID             string
	APIKey         string
	APIEndpoint    string
	DefaultHeaders map[string]string
	Models         []Model
}
