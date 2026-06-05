package agent

import "testing"

func TestIsContextOverflow(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		expected bool
	}{
		// Empty/no error
		{"empty", "", false},

		// Anthropic
		{"anthropic_token", "prompt is too long: 213462 tokens > 200000 maximum", true},
		{"anthropic_413", `413 {"error":{"type":"request_too_large","message":"Request exceeds the maximum size"}}`, true},

		// OpenAI
		{"openai", "Your input exceeds the context window of this model", true},
		{"openai_litellm", "Requested token count exceeds the model's maximum context length of 131072 tokens", true},

		// Google
		{"google", "The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)", true},

		// xAI
		{"xai", "This model's maximum prompt length is 131072 but the request contains 537812 tokens", true},

		// Groq
		{"groq", "Please reduce the length of the messages or completion", true},

		// OpenRouter
		{"openrouter", "This endpoint's maximum context length is 128000 tokens. However, you requested about 200000 tokens", true},
		{"openrouter_poolside", "Input length 200000 exceeds the maximum allowed input length of 128000 tokens.", true},

		// Together AI
		{"together", "The input (200000 tokens) is longer than the model's context length (128000 tokens).", true},

		// llama.cpp
		{"llamacpp", "the request exceeds the available context size, try increasing it", true},

		// LM Studio
		{"lmstudio", "tokens to keep from the initial prompt is greater than the context length", true},

		// GitHub Copilot
		{"copilot", "prompt token count of 200000 exceeds the limit of 128000", true},

		// MiniMax
		{"minimax", "invalid params, context window exceeds limit", true},

		// Kimi
		{"kimi", "Your request exceeded model token limit: 128000 (requested: 200000)", true},

		// Mistral
		{"mistral", "Prompt contains 200000 tokens ... too large for model with 128000 maximum context length", true},

		// Ollama
		{"ollama", "prompt too long; exceeded max context length by 50000 tokens", true},

		// Generic
		{"generic_context_length_exceeded", "context_length_exceeded", true},
		{"generic_too_many_tokens", "too many tokens", true},
		{"generic_token_limit", "token limit exceeded", true},

		// Cerebras
		{"cerebras_400", "400 status code (no body)", true},
		{"cerebras_413", "413 status code (no body)", true},

		// Non-overflow patterns (should NOT match)
		{"rate_limit", "rate limit exceeded", false},
		{"too_many_requests", "too many requests", false},
		{"bedrock_throttling", "Throttling error: Too many tokens, please wait before trying again.", false},
		{"bedrock_unavailable", "Service unavailable: Please try again later", false},

		// Unrelated errors
		{"network_error", "connection refused", false},
		{"auth_error", "invalid API key", false},
		{"generic_error", "something went wrong", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := IsContextOverflow(tt.input)
			if got != tt.expected {
				t.Fatalf("IsContextOverflow(%q) = %v, want %v", tt.input, got, tt.expected)
			}
		})
	}
}
