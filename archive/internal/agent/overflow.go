package agent

import "regexp"

// overflowPattern matches context overflow errors from different providers.
// These patterns match error messages returned when the input exceeds
// the model's context window.
//
// Tested provider messages:
//   - Anthropic: "prompt is too long: 213462 tokens > 200000 maximum"
//   - Anthropic: "413 {"error":{"type":"request_too_large",...}}"
//   - OpenAI: "Your input exceeds the context window of this model"
//   - OpenAI/LiteLLM: "Requested token count exceeds the model's maximum context length of 131072 tokens"
//   - Google: "The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)"
//   - xAI: "This model's maximum prompt length is 131072 but the request contains 537812 tokens"
//   - Groq: "Please reduce the length of the messages or completion"
//   - OpenRouter: "This endpoint's maximum context length is X tokens. However, you requested about Y tokens"
//   - OpenRouter/Poolside: "Input length X exceeds the maximum allowed input length of Y tokens."
//   - Together AI: "The input (X tokens) is longer than the model's context length (Y tokens)."
//   - llama.cpp: "the request exceeds the available context size, try increasing it"
//   - LM Studio: "tokens to keep from the initial prompt is greater than the context length"
//   - GitHub Copilot: "prompt token count of X exceeds the limit of Y"
//   - MiniMax: "invalid params, context window exceeds limit"
//   - Kimi: "Your request exceeded model token limit: X (requested: Y)"
//   - Cerebras: "400/413 status code (no body)"
//   - Mistral: "Prompt contains X tokens ... too large for model with Y maximum context length"
//   - Ollama: "prompt too long; exceeded max context length by X tokens"
var overflowPatterns = []*regexp.Regexp{
	regexp.MustCompile(`(?i)prompt is too long`),                          // Anthropic
	regexp.MustCompile(`(?i)request_too_large`),                           // Anthropic HTTP 413
	regexp.MustCompile(`(?i)input is too long for requested model`),       // Amazon Bedrock
	regexp.MustCompile(`(?i)exceeds the context window`),                  // OpenAI
	regexp.MustCompile(`(?i)exceeds (?:the )?(?:model'?s )?maximum context length of [\d,]+ tokens?`), // LiteLLM
	regexp.MustCompile(`(?i)input token count.*exceeds the maximum`),      // Google Gemini
	regexp.MustCompile(`(?i)maximum prompt length is \d+`),                // xAI
	regexp.MustCompile(`(?i)reduce the length of the messages`),           // Groq
	regexp.MustCompile(`(?i)maximum context length is \d+ tokens`),        // OpenRouter
	regexp.MustCompile(`(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?`), // OpenRouter/Poolside
	regexp.MustCompile(`(?i)input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)`), // Together AI
	regexp.MustCompile(`(?i)exceeds the limit of \d+`),                    // GitHub Copilot
	regexp.MustCompile(`(?i)exceeds the available context size`),          // llama.cpp
	regexp.MustCompile(`(?i)greater than the context length`),             // LM Studio
	regexp.MustCompile(`(?i)context window exceeds limit`),                // MiniMax
	regexp.MustCompile(`(?i)exceeded model token limit`),                  // Kimi
	regexp.MustCompile(`(?i)too large for model with \d+ maximum context length`), // Mistral
	regexp.MustCompile(`(?i)prompt too long; exceeded (?:max )?context length`),    // Ollama
	regexp.MustCompile(`(?i)context[_ ]length[_ ]exceeded`),               // Generic
	regexp.MustCompile(`(?i)too many tokens`),                             // Generic
	regexp.MustCompile(`(?i)token limit exceeded`),                        // Generic
	regexp.MustCompile(`(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)`), // Cerebras
}

// nonOverflowPatterns exclude messages that look like overflow but aren't.
// e.g. Bedrock throttling: "ThrottlingException: Too many tokens, please wait..."
var nonOverflowPatterns = []*regexp.Regexp{
	regexp.MustCompile(`(?i)^(?:Throttling error|Service unavailable):`), // AWS Bedrock
	regexp.MustCompile(`(?i)rate limit`),                                  // Generic rate limiting
	regexp.MustCompile(`(?i)too many requests`),                           // HTTP 429
}

// IsContextOverflow checks if an error message indicates a context overflow.
// Returns true if the error matches known provider overflow patterns.
func IsContextOverflow(errorMessage string) bool {
	if errorMessage == "" {
		return false
	}

	for _, p := range nonOverflowPatterns {
		if p.MatchString(errorMessage) {
			return false
		}
	}

	for _, p := range overflowPatterns {
		if p.MatchString(errorMessage) {
			return true
		}
	}

	return false
}
