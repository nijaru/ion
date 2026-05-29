package llm

import (
	"testing"
)

func TestDetectCompatOpenRouter(t *testing.T) {
	compat := DetectCompat("openrouter", "https://openrouter.ai/api/v1")
	if compat.ThinkingFormat != ThinkingFormatOpenRouter {
		t.Fatalf("ThinkingFormat = %q, want %q", compat.ThinkingFormat, ThinkingFormatOpenRouter)
	}
	if !compat.SupportsReasoningEffort {
		t.Fatal("OpenRouter should support reasoning effort")
	}
}

func TestDetectCompatDeepSeek(t *testing.T) {
	compat := DetectCompat("deepseek", "https://api.deepseek.com/v1")
	if compat.ThinkingFormat != ThinkingFormatDeepSeek {
		t.Fatalf("ThinkingFormat = %q, want %q", compat.ThinkingFormat, ThinkingFormatDeepSeek)
	}
	if !compat.RequiresReasoningContentOnAssistantMessages {
		t.Fatal("DeepSeek should require reasoning_content on assistant messages")
	}
}

func TestDetectCompatTogether(t *testing.T) {
	compat := DetectCompat("together", "https://api.together.xyz/v1")
	if compat.ThinkingFormat != ThinkingFormatTogether {
		t.Fatalf("ThinkingFormat = %q, want %q", compat.ThinkingFormat, ThinkingFormatTogether)
	}
	if compat.SupportsReasoningEffort {
		t.Fatal("Together should not support reasoning_effort")
	}
	if compat.MaxTokensField != "max_tokens" {
		t.Fatalf("MaxTokensField = %q, want %q", compat.MaxTokensField, "max_tokens")
	}
}

func TestDetectCompatZai(t *testing.T) {
	compat := DetectCompat("zai", "https://api.z.ai/api/paas/v4")
	if compat.ThinkingFormat != ThinkingFormatZai {
		t.Fatalf("ThinkingFormat = %q, want %q", compat.ThinkingFormat, ThinkingFormatZai)
	}
	if compat.SupportsReasoningEffort {
		t.Fatal("Z.ai should not support reasoning_effort")
	}
}

func TestDetectCompatOpenAI(t *testing.T) {
	compat := DetectCompat("openai", "https://api.openai.com/v1")
	if compat.ThinkingFormat != ThinkingFormatOpenAI {
		t.Fatalf("ThinkingFormat = %q, want %q", compat.ThinkingFormat, ThinkingFormatOpenAI)
	}
	if !compat.SupportsReasoningEffort {
		t.Fatal("OpenAI should support reasoning effort")
	}
	if !compat.SupportsStore {
		t.Fatal("OpenAI should support store")
	}
	if !compat.SupportsDeveloperRole {
		t.Fatal("OpenAI should support developer role")
	}
	if compat.MaxTokensField != "max_completion_tokens" {
		t.Fatalf("MaxTokensField = %q, want %q", compat.MaxTokensField, "max_completion_tokens")
	}
}

func TestDetectCompatGrok(t *testing.T) {
	compat := DetectCompat("xai", "https://api.x.ai/v1")
	if compat.ThinkingFormat != ThinkingFormatOpenAI {
		t.Fatalf("ThinkingFormat = %q, want %q", compat.ThinkingFormat, ThinkingFormatOpenAI)
	}
	if compat.SupportsReasoningEffort {
		t.Fatal("Grok should not support reasoning_effort")
	}
	if compat.SupportsStore {
		t.Fatal("Grok should not support store")
	}
}

func TestDetectCompatMoonshot(t *testing.T) {
	compat := DetectCompat("moonshotai", "https://api.moonshot.cn/v1")
	if compat.SupportsReasoningEffort {
		t.Fatal("Moonshot should not support reasoning_effort")
	}
	if compat.MaxTokensField != "max_tokens" {
		t.Fatalf("MaxTokensField = %q, want %q", compat.MaxTokensField, "max_tokens")
	}
}

func TestMergeCompat(t *testing.T) {
	detected := DefaultProviderCompat()
	override := ProviderCompat{
		ThinkingFormat: ThinkingFormatOpenRouter,
		MaxTokensField: "max_tokens",
	}

	merged := MergeCompat(detected, override)
	if merged.ThinkingFormat != ThinkingFormatOpenRouter {
		t.Fatalf("ThinkingFormat = %q, want %q", merged.ThinkingFormat, ThinkingFormatOpenRouter)
	}
	if merged.MaxTokensField != "max_tokens" {
		t.Fatalf("MaxTokensField = %q, want %q", merged.MaxTokensField, "max_tokens")
	}
	// Non-overridden fields should keep detected values
	if !merged.SupportsStore {
		t.Fatal("SupportsStore should be preserved from detected")
	}
}
