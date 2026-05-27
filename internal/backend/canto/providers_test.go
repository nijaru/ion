package canto

import (
	"context"
	"testing"

	"github.com/nijaru/canto/llm"
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

func TestProviderModelsAttachesReasoningCapsFromMetadata(t *testing.T) {
	// Without metadata, model should have no capabilities attached
	models := providerModels(&config.Config{
		Provider: "openrouter",
		Model:    "qwen/qwen3-235b-a22b",
	})
	if len(models) != 1 {
		t.Fatalf("models len = %d, want 1", len(models))
	}
	// No metadata cached, so capabilities should be nil
	if models[0].Capabilities != nil {
		t.Fatalf("expected nil capabilities without metadata, got %#v", models[0].Capabilities)
	}
}

func TestOpenAICompatibleModelCapsRespectsConfigOverrides(t *testing.T) {
	tempFalse := false

	cfg := &config.Config{
		Provider: "openai-compatible",
		Model:    "my-special-mimo-v3-model",
		Models: []config.ModelDef{
			{
				Pattern:     "*mimo*",
				Preset:      "reasoning",
				Temperature: &tempFalse,
				SystemRole:  "developer",
			},
		},
	}

	_, _ = newProvider(context.Background(), cfg)

	caps := llm.ResolveCapabilities("my-special-mimo-v3-model")

	if caps.Temperature {
		t.Fatal("expected temperature to be overridden to false")
	}
	if caps.Reasoning.Kind != llm.ReasoningKindEffort {
		t.Fatalf("reasoning kind = %q, want effort", caps.Reasoning.Kind)
	}
	if caps.SystemRole != llm.RoleDeveloper {
		t.Fatalf("system role = %q, want developer", caps.SystemRole)
	}
}
