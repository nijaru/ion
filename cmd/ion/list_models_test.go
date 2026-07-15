package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestResolveListModelsSearch(t *testing.T) {
	if got, err := resolveListModelsSearch(false, []string{"ignored"}); err != nil || got != "" {
		t.Fatalf("unrequested search = %q, err=%v", got, err)
	}
	if got, err := resolveListModelsSearch(true, nil); err != nil || got != "" {
		t.Fatalf("empty search = %q, err=%v", got, err)
	}
	if got, err := resolveListModelsSearch(true, []string{"gpt 5"}); err != nil || got != "gpt 5" {
		t.Fatalf("search = %q, err=%v", got, err)
	}
	if _, err := resolveListModelsSearch(true, []string{"one", "two"}); err == nil {
		t.Fatal("expected multiple search patterns to fail")
	}
	got, openResumePicker := normalizeFlagArgs([]string{"--list-models", "gpt"})
	if openResumePicker || len(got) != 2 || got[0] != "--list-models" || got[1] != "gpt" {
		t.Fatalf("normalized list-models args = %#v, picker=%v", got, openResumePicker)
	}
}

func TestFuzzyModelMatchUsesProviderAndModel(t *testing.T) {
	if !fuzzyModelMatch("oa gpt5", "openai gpt-5.4") {
		t.Fatal("expected provider/model subsequence to match")
	}
	if fuzzyModelMatch("anthropic", "openai gpt-5.4") {
		t.Fatal("unexpected provider mismatch")
	}
	if !fuzzyModelMatch("gpt/5.4", "openai gpt-5.4") {
		t.Fatal("expected slash-separated tokens to match")
	}
}

func TestFilterListModelsPreservesPiOrderingInput(t *testing.T) {
	models := []llm.ModelMetadata{
		{Provider: "openai", ID: "gpt-5.4"},
		{Provider: "anthropic", ID: "claude-sonnet"},
	}
	filtered := filterListModels(models, "gpt")
	if len(filtered) != 1 || filtered[0].ID != "gpt-5.4" {
		t.Fatalf("filtered = %#v", filtered)
	}
	if models[0].ID != "gpt-5.4" || models[1].ID != "claude-sonnet" {
		t.Fatalf("filter mutated input: %#v", models)
	}
}

func TestWriteModelTableIncludesPiListingMetadata(t *testing.T) {
	var out bytes.Buffer
	writeModelTable(&out, []llm.ModelMetadata{
		{
			Provider:     "openrouter",
			ID:           "anthropic/claude",
			ContextLimit: 200000,
			MaxTokens:    8192,
			Reasoning:    true,
			Input:        []string{"text", "image"},
		},
	})

	got := out.String()
	for _, want := range []string{
		"provider    model             context  max-out  thinking  images",
		"openrouter  anthropic/claude  200K     8.2K     yes       yes",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("table = %q, missing %q", got, want)
		}
	}
}

func TestFormatModelTokenCount(t *testing.T) {
	tests := map[int]string{
		0:       "0",
		999:     "999",
		1000:    "1K",
		128000:  "128K",
		1500000: "1.5M",
	}
	for input, want := range tests {
		if got := formatModelTokenCount(input); got != want {
			t.Errorf("formatModelTokenCount(%d) = %q, want %q", input, got, want)
		}
	}
}
