package llm

import "strings"

// ThinkingFormat controls how reasoning/thinking parameters are sent to providers.
// Each provider has a different format for controlling reasoning behavior.
type ThinkingFormat string

const (
	// ThinkingFormatOpenAI uses top-level reasoning_effort field.
	// Used by: OpenAI, Groq, Cerebras, Fireworks, Mistral, xAI
	ThinkingFormatOpenAI ThinkingFormat = "openai"

	// ThinkingFormatOpenRouter uses nested reasoning: { effort: "..." } object.
	// Used by: OpenRouter
	ThinkingFormatOpenRouter ThinkingFormat = "openrouter"

	// ThinkingFormatDeepSeek uses thinking: { type: "enabled"/"disabled" } plus reasoning_effort.
	// Used by: DeepSeek
	ThinkingFormatDeepSeek ThinkingFormat = "deepseek"

	// ThinkingFormatTogether uses reasoning: { enabled: bool } plus reasoning_effort.
	// Used by: Together AI
	ThinkingFormatTogether ThinkingFormat = "together"

	// ThinkingFormatZai uses top-level enable_thinking boolean.
	// Used by: Z.ai
	ThinkingFormatZai ThinkingFormat = "zai"

	// ThinkingFormatQwen uses top-level enable_thinking boolean.
	// Used by: Qwen models
	ThinkingFormatQwen ThinkingFormat = "qwen"

	// ThinkingFormatQwenChatTemplate uses chat_template_kwargs.enable_thinking.
	// Used by: Qwen models via vLLM
	ThinkingFormatQwenChatTemplate ThinkingFormat = "qwen-chat-template"

	// ThinkingFormatNone means no thinking/reasoning support.
	ThinkingFormatNone ThinkingFormat = ""
)

// ProviderCompat describes provider-specific compatibility settings.
// These control how requests are formatted for different providers.
type ProviderCompat struct {
	// ThinkingFormat controls how reasoning parameters are sent.
	ThinkingFormat ThinkingFormat

	// SupportsReasoningEffort indicates whether the provider supports reasoning_effort.
	SupportsReasoningEffort bool

	// MaxTokensField is the JSON field name for max tokens.
	// "max_tokens" or "max_completion_tokens".
	MaxTokensField string

	// SupportsStore indicates whether the provider supports the store field.
	SupportsStore bool

	// SupportsDeveloperRole indicates whether the provider supports the developer role.
	SupportsDeveloperRole bool

	// SupportsStrictMode indicates whether the provider supports strict mode in tool definitions.
	SupportsStrictMode bool

	// RequiresToolResultName indicates whether tool results need the name field.
	RequiresToolResultName bool

	// RequiresAssistantAfterToolResult indicates whether an assistant message is needed after tool results.
	RequiresAssistantAfterToolResult bool

	// RequiresThinkingAsText indicates whether thinking blocks must be converted to text blocks.
	RequiresThinkingAsText bool

	// RequiresReasoningContentOnAssistantMessages indicates whether assistant messages need reasoning_content field.
	RequiresReasoningContentOnAssistantMessages bool
}

// DefaultProviderCompat returns the default compatibility settings for OpenAI-compatible providers.
func DefaultProviderCompat() ProviderCompat {
	return ProviderCompat{
		ThinkingFormat:       ThinkingFormatOpenAI,
		SupportsReasoningEffort: true,
		MaxTokensField:       "max_completion_tokens",
		SupportsStore:        true,
		SupportsDeveloperRole: true,
		SupportsStrictMode:   true,
	}
}

// DetectCompat auto-detects compatibility settings from provider name and base URL.
// This matches Pi's detectCompat() logic.
func DetectCompat(provider, baseURL string) ProviderCompat {
	compat := DefaultProviderCompat()

	isZai := provider == "zai" || strings.Contains(baseURL, "api.z.ai")
	isTogether := provider == "together" ||
		strings.Contains(baseURL, "api.together.ai") ||
		strings.Contains(baseURL, "api.together.xyz")
	isMoonshot := provider == "moonshotai" ||
		provider == "moonshotai-cn" ||
		strings.Contains(baseURL, "api.moonshot.")
	isGrok := provider == "xai" || strings.Contains(baseURL, "api.x.ai")
	isDeepSeek := provider == "deepseek" || strings.Contains(baseURL, "deepseek.com")
	isCloudflare := strings.Contains(baseURL, "gateway.ai.cloudflare.com") ||
		strings.Contains(baseURL, "api.cloudflare.com")

	// Non-standard providers that don't support certain OpenAI features
	isNonStandard := isGrok || isZai || isTogether || isMoonshot || isDeepSeek ||
		isCloudflare || strings.Contains(baseURL, "cerebras.ai") ||
		strings.Contains(baseURL, "chutes.ai")

	compat.SupportsStore = !isNonStandard
	compat.SupportsDeveloperRole = !isNonStandard
	compat.SupportsStrictMode = !isMoonshot && !isTogether && !isCloudflare

	// MaxTokensField: some providers need max_tokens instead of max_completion_tokens
	useMaxTokens := strings.Contains(baseURL, "chutes.ai") || isMoonshot || isCloudflare || isTogether
	if useMaxTokens {
		compat.MaxTokensField = "max_tokens"
	}

	// Reasoning effort support
	compat.SupportsReasoningEffort = !isGrok && !isZai && !isMoonshot && !isTogether && !isCloudflare

	// ThinkingFormat detection
	switch {
	case isDeepSeek:
		compat.ThinkingFormat = ThinkingFormatDeepSeek
		compat.RequiresReasoningContentOnAssistantMessages = true
	case isZai:
		compat.ThinkingFormat = ThinkingFormatZai
	case isTogether:
		compat.ThinkingFormat = ThinkingFormatTogether
	case provider == "openrouter" || strings.Contains(baseURL, "openrouter.ai"):
		compat.ThinkingFormat = ThinkingFormatOpenRouter
	default:
		compat.ThinkingFormat = ThinkingFormatOpenAI
	}

	return compat
}

// MergeCompat merges explicit model-level overrides with detected compat settings.
// Model-level overrides take precedence over detected settings.
func MergeCompat(detected, override ProviderCompat) ProviderCompat {
	result := detected

	if override.ThinkingFormat != ThinkingFormatNone {
		result.ThinkingFormat = override.ThinkingFormat
	}
	if override.MaxTokensField != "" {
		result.MaxTokensField = override.MaxTokensField
	}

	// Boolean fields: only override if explicitly set to true
	// This prevents zero-value overrides from clobbering detected values
	if override.SupportsStore {
		result.SupportsStore = true
	}
	if override.SupportsDeveloperRole {
		result.SupportsDeveloperRole = true
	}
	if override.SupportsStrictMode {
		result.SupportsStrictMode = true
	}
	if override.SupportsReasoningEffort {
		result.SupportsReasoningEffort = true
	}
	if override.RequiresToolResultName {
		result.RequiresToolResultName = true
	}
	if override.RequiresAssistantAfterToolResult {
		result.RequiresAssistantAfterToolResult = true
	}
	if override.RequiresThinkingAsText {
		result.RequiresThinkingAsText = true
	}
	if override.RequiresReasoningContentOnAssistantMessages {
		result.RequiresReasoningContentOnAssistantMessages = true
	}

	return result
}
