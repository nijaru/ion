package registry

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"github.com/nijaru/ion/internal/config"
)

func TestListModelsCachesProviderModels(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	providerModelsOnce = sync.Once{}
	providerModelsCacheMap = nil

	oldFetcher := providerCatalogFetcher
	defer func() { providerCatalogFetcher = oldFetcher }()

	var calls int
	providerCatalogFetcher = func(ctx context.Context, provider string, cfg *config.Config) ([]ModelMetadata, error) {
		calls++
		if provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", provider)
		}
		return []ModelMetadata{
			{ID: "z-model", ContextLimit: 32000, InputPrice: 0.5, OutputPrice: 1.0},
			{ID: "a-model", ContextLimit: 128000, InputPrice: 0.1, OutputPrice: 0.2},
		}, nil
	}

	items, err := ListModels(context.Background(), "openrouter")
	if err != nil {
		t.Fatalf("first ListModels: %v", err)
	}
	if len(items) != 2 {
		t.Fatalf("items len = %d, want 2", len(items))
	}
	if calls != 1 {
		t.Fatalf("fetch calls = %d, want 1", calls)
	}

	items, err = ListModels(context.Background(), "openrouter")
	if err != nil {
		t.Fatalf("second ListModels: %v", err)
	}
	if calls != 1 {
		t.Fatalf("fetch calls = %d, want cached result", calls)
	}
	if got := items[0].ID; got != "a-model" {
		t.Fatalf("cached items not sorted: %#v", items)
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "data", "models_cache.json")); err != nil {
		t.Fatalf("expected cache file: %v", err)
	}
}

func TestFetchModelsUsesDirectFetcherForNativeProviders(t *testing.T) {
	tests := []struct {
		provider string
		target   *func(context.Context) ([]ModelMetadata, error)
	}{
		{provider: "anthropic", target: &anthropicFetcher},
		{provider: "openai", target: &openAIFetcher},
		{provider: "openrouter", target: &openRouterFetcher},
		{provider: "gemini", target: &geminiFetcher},
		{provider: "ollama", target: &ollamaFetcher},
	}

	for _, tc := range tests {
		t.Run(tc.provider, func(t *testing.T) {
			called := false
			original := *tc.target
			*tc.target = func(ctx context.Context) ([]ModelMetadata, error) {
				called = true
				return []ModelMetadata{{ID: tc.provider + "-model"}}, nil
			}
			defer func() { *tc.target = original }()

			models, err := fetchModels(context.Background(), tc.provider, &config.Config{Provider: tc.provider})
			if err != nil {
				t.Fatalf("fetchModels(%q): %v", tc.provider, err)
			}
			if !called {
				t.Fatalf("direct fetcher for %q was not called", tc.provider)
			}
			if len(models) != 1 || models[0].ID != tc.provider+"-model" {
				t.Fatalf("models = %#v", models)
			}
		})
	}
}

func TestFetchModelsUsesCatwalkOnlyWhenConfigured(t *testing.T) {
	t.Setenv("CATWALK_URL", "https://catalog.example")

	original := catwalkFetcher
	defer func() { catwalkFetcher = original }()

	called := false
	catwalkFetcher = func(ctx context.Context, provider string) ([]ModelMetadata, error) {
		called = true
		if provider != "mystery" {
			t.Fatalf("provider = %q, want mystery", provider)
		}
		return []ModelMetadata{{ID: "mystery-model"}}, nil
	}

	models, err := fetchModels(context.Background(), "mystery", &config.Config{Provider: "mystery"})
	if err != nil {
		t.Fatalf("fetchModels fallback: %v", err)
	}
	if !called {
		t.Fatal("expected catwalk fallback to be called")
	}
	if len(models) != 1 || models[0].ID != "mystery-model" {
		t.Fatalf("models = %#v", models)
	}
}

func TestFetchModelsRejectsUnknownProviderWithoutCatalog(t *testing.T) {
	t.Setenv("CATWALK_URL", "")

	_, err := fetchModels(context.Background(), "mystery", &config.Config{Provider: "mystery"})
	if err == nil {
		t.Fatal("expected unknown provider without configured catalog to fail")
	}
	if got := err.Error(); got != "no live model catalog configured for provider mystery" {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestNormalizeOllamaBaseURL(t *testing.T) {
	tests := map[string]string{
		"":                          "http://127.0.0.1:11434",
		"localhost:11434":           "http://localhost:11434",
		"http://localhost:11434/":   "http://localhost:11434",
		"https://remote.example/v1": "https://remote.example/v1",
	}
	for input, want := range tests {
		t.Run(fmt.Sprintf("%q", input), func(t *testing.T) {
			if got := normalizeOllamaBaseURL(input); got != want {
				t.Fatalf("normalizeOllamaBaseURL(%q) = %q, want %q", input, got, want)
			}
		})
	}
}
