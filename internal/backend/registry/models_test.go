package registry

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/config"
)

func TestListModelsCachesProviderModels(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	providerModelsOnce = sync.Once{}
	providerModelsCacheMap = nil

	oldFetcher := providerCatalogFetcher
	oldModelsDevFetcher := modelsDevFetcher
	defer func() { providerCatalogFetcher = oldFetcher }()
	defer func() { modelsDevFetcher = oldModelsDevFetcher }()
	modelsDevFetcher = func(ctx context.Context) (map[string]int64, error) {
		return map[string]int64{}, nil
	}

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

func TestListModelsForConfigRejectsNilConfig(t *testing.T) {
	_, err := ListModelsForConfig(t.Context(), nil)
	if err == nil || err.Error() != "model provider config is required" {
		t.Fatalf("ListModelsForConfig(nil) error = %v", err)
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

func TestFetchModelsUsesConfiguredOpenAICompatibleEndpoint(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/models" {
			t.Fatalf("path = %q, want /models", r.URL.Path)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"data":[{"id":"custom/model-a"},{"id":"custom/model-b"}]}`))
	}))
	defer server.Close()

	models, err := fetchModels(context.Background(), "openai-compatible", &config.Config{
		Provider: "openai-compatible",
		Endpoint: server.URL,
	})
	if err != nil {
		t.Fatalf("fetchModels custom endpoint: %v", err)
	}
	if len(models) != 2 {
		t.Fatalf("models = %#v", models)
	}
}

func TestFetchModelsRejectsUnknownProviderWithoutCatalog(t *testing.T) {
	_, err := fetchModels(context.Background(), "mystery", &config.Config{Provider: "mystery"})
	if err == nil {
		t.Fatal("expected unknown provider without configured catalog to fail")
	}
	if got := err.Error(); got != "no model listing available for provider mystery" {
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

func TestSortModelsUsesOrgThenNewest(t *testing.T) {
	models := []ModelMetadata{
		{ID: "z-ai/glm-4.5", Created: time.Date(2025, 2, 1, 0, 0, 0, 0, time.UTC).Unix()},
		{ID: "openai/gpt-4.1", Created: time.Date(2025, 4, 1, 0, 0, 0, 0, time.UTC).Unix()},
		{ID: "z-ai/glm-5", Created: time.Date(2025, 5, 1, 0, 0, 0, 0, time.UTC).Unix()},
	}
	sortModels(models)
	got := []string{models[0].ID, models[1].ID, models[2].ID}
	want := []string{"openai/gpt-4.1", "z-ai/glm-5", "z-ai/glm-4.5"}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("sortModels order = %#v, want %#v", got, want)
		}
	}
}

func TestInferCreatedFromModelID(t *testing.T) {
	if got := inferCreatedFromModelID("openai/gpt-4.1"); got != 0 {
		t.Fatalf("expected no inferred date, got %d", got)
	}
	if got := inferCreatedFromModelID("mistralai/mistral-large-2512"); got == 0 {
		t.Fatal("expected YYMM token to infer a created timestamp")
	}
	if got := inferCreatedFromModelID("anthropic/claude-2025-03-25"); got == 0 {
		t.Fatal("expected YYYY-MM-DD token to infer a created timestamp")
	}
}
