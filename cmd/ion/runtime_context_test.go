package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

func TestApplyModelMetadataCapabilities(t *testing.T) {
	caps := llm.DefaultCapabilities()
	applyModelMetadataCapabilities(&caps, llm.ModelMetadata{
		Input:     []string{"text"},
		Reasoning: true,
	})
	if caps.SupportsImages() {
		t.Fatal("text-only metadata still accepts images")
	}
	if caps.Reasoning.Kind != llm.ReasoningKindEffort || !caps.SupportsReasoningEffort("high") {
		t.Fatalf("reasoning capabilities = %#v, want effort control", caps.Reasoning)
	}

	caps.Reasoning.Kind = llm.ReasoningKindBudget
	applyModelMetadataCapabilities(&caps, llm.ModelMetadata{Reasoning: true})
	if caps.Reasoning.Kind != llm.ReasoningKindBudget {
		t.Fatalf("explicit reasoning capability was replaced: %#v", caps.Reasoning)
	}
}

func TestResolvedContextWindowPrefersConfigThenCachedMetadata(t *testing.T) {
	dataDir := t.TempDir()
	cache := map[string]llm.ModelMetadata{
		"openai/cached-model": {
			Provider:     "openai",
			ID:           "cached-model",
			ContextLimit: 128000,
			UpdatedAt:    time.Now().Unix(),
		},
	}
	data, err := json.Marshal(cache)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dataDir, "metadata_cache.json"), data, 0o600); err != nil {
		t.Fatal(err)
	}
	catalog := llm.NewModelCatalog(llm.ModelCatalogOptions{DataDir: dataDir})
	info := app.NewSetupRuntime(&config.Config{}, "setup")

	cfg := &config.Config{Provider: "openai", Model: "cached-model"}
	if got := resolvedContextWindow(cfg, info, catalog); got != 128000 {
		t.Fatalf("cached context window = %d, want 128000", got)
	}
	cfg.ContextLimit = 32000
	if got := resolvedContextWindow(cfg, info, catalog); got != 32000 {
		t.Fatalf("configured context window = %d, want 32000", got)
	}
}
