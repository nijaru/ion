package registry

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/config"
)

func TestResolveFastPresetRequiresExplicitModel(t *testing.T) {
	_, err := ResolveRuntimeConfig(context.Background(), &config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
	}, PresetFast)
	if err == nil {
		t.Fatal("expected missing fast model to fail")
	}
	if !strings.Contains(err.Error(), "fast model is not configured") {
		t.Fatalf("error = %q, want missing fast model guidance", err)
	}
}

func TestResolveFastPresetUsesConfiguredModel(t *testing.T) {
	cfg, err := ResolveRuntimeConfig(context.Background(), &config.Config{
		Provider:            "openai",
		Model:               "gpt-4.1",
		ReasoningEffort:     "high",
		FastModel:           "gpt-4.1-mini",
		FastReasoningEffort: "",
	}, PresetFast)
	if err != nil {
		t.Fatalf("resolve fast preset: %v", err)
	}
	if cfg.Model != "gpt-4.1-mini" {
		t.Fatalf("model = %q, want gpt-4.1-mini", cfg.Model)
	}
	if cfg.ReasoningEffort != "low" {
		t.Fatalf("reasoning_effort = %q, want low default", cfg.ReasoningEffort)
	}
}

func TestResolveSummaryPresetDoesNotInferFastModel(t *testing.T) {
	cfg, err := ResolveRuntimeConfig(context.Background(), &config.Config{
		Provider: "openrouter",
		Model:    "anthropic/claude-sonnet-4.5",
	}, PresetSummary)
	if err != nil {
		t.Fatalf("resolve summary preset: %v", err)
	}
	if cfg.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("model = %q, want primary model fallback", cfg.Model)
	}
	if cfg.ReasoningEffort != "low" {
		t.Fatalf("reasoning_effort = %q, want low", cfg.ReasoningEffort)
	}
}

func TestResolveSummaryPresetPrefersConfiguredFastModel(t *testing.T) {
	cfg, err := ResolveRuntimeConfig(context.Background(), &config.Config{
		Provider:  "openrouter",
		Model:     "anthropic/claude-sonnet-4.5",
		FastModel: "google/gemini-2.0-flash-lite-001",
	}, PresetSummary)
	if err != nil {
		t.Fatalf("resolve summary preset: %v", err)
	}
	if cfg.Model != "google/gemini-2.0-flash-lite-001" {
		t.Fatalf("model = %q, want configured fast model", cfg.Model)
	}
	if cfg.ReasoningEffort != "low" {
		t.Fatalf("reasoning_effort = %q, want low", cfg.ReasoningEffort)
	}
}
