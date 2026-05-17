package canto

import (
	"testing"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
)

func TestResolvedAPIKeyDoesNotUseCustomAuthForDefaultProvider(t *testing.T) {
	t.Setenv("LOCAL_API_KEY", "local-key")
	t.Setenv("OPENROUTER_API_KEY", "router-key")

	def, ok := providers.Lookup("openrouter")
	if !ok {
		t.Fatal("openrouter definition missing")
	}
	cfg := &config.Config{
		Provider:   "openrouter",
		AuthEnvVar: "LOCAL_API_KEY",
	}

	if got := providers.ResolvedAuthToken(cfg, def); got != "router-key" {
		t.Fatalf("api key = %q, want default provider key", got)
	}
	if got := providers.MissingAuthDetail(cfg, def); got != "OPENROUTER_API_KEY" {
		t.Fatalf("missing auth detail = %q, want OPENROUTER_API_KEY", got)
	}
}

func TestResolvedAPIKeyUsesCustomAuthForCustomProvider(t *testing.T) {
	t.Setenv("LOCAL_API_KEY", "local-key")
	t.Setenv("OPENAI_COMPATIBLE_API_KEY", "fallback-key")

	def, ok := providers.Lookup("openai-compatible")
	if !ok {
		t.Fatal("custom provider definition missing")
	}
	cfg := &config.Config{
		Provider:   "openai-compatible",
		AuthEnvVar: "LOCAL_API_KEY",
	}

	if got := providers.ResolvedAuthToken(cfg, def); got != "local-key" {
		t.Fatalf("api key = %q, want custom provider key", got)
	}
	if got := providers.MissingAuthDetail(cfg, def); got != "LOCAL_API_KEY" {
		t.Fatalf("missing auth detail = %q, want LOCAL_API_KEY", got)
	}
}

func TestProviderModelsUsesConfiguredContextLimitWithoutDiscovery(t *testing.T) {
	models := providerModels(&config.Config{
		Provider:     "local-api",
		Model:        "qwen3.6:27b",
		ContextLimit: 70000,
	})
	if len(models) != 1 {
		t.Fatalf("models len = %d, want 1", len(models))
	}
	if models[0].ID != "qwen3.6:27b" {
		t.Fatalf("model ID = %q, want qwen3.6:27b", models[0].ID)
	}
	if models[0].ContextWindow != 70000 {
		t.Fatalf("context window = %d, want configured limit", models[0].ContextWindow)
	}
}
