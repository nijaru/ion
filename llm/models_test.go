package llm

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
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

func TestQueryModelsForConfigReportsStaleCacheExplicitly(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	providerModelsOnce = sync.Once{}
	providerModelsCacheMap = nil

	oldFetcher := providerCatalogFetcher
	t.Cleanup(func() { providerCatalogFetcher = oldFetcher })
	calls := 0
	providerCatalogFetcher = func(ctx context.Context, provider string, cfg *config.Config) ([]ModelMetadata, error) {
		calls++
		if calls == 1 {
			return []ModelMetadata{{ID: "cached-model", Provider: provider}}, nil
		}
		return nil, errors.New("catalog offline")
	}

	cfg := &config.Config{Provider: "openrouter"}
	first, err := QueryModelsForConfig(t.Context(), cfg)
	if err != nil {
		t.Fatalf("first query: %v", err)
	}
	if first.Stale {
		t.Fatal("fresh catalog was marked stale")
	}

	providerModelsMu.Lock()
	key := providerCacheKey(cfg)
	cached := providerModelsCacheMap[key]
	cached.UpdatedAt = time.Now().Add(-2 * time.Hour).Unix()
	providerModelsCacheMap[key] = cached
	providerModelsMu.Unlock()

	second, err := QueryModelsForConfig(t.Context(), cfg)
	if err != nil {
		t.Fatalf("stale query: %v", err)
	}
	if !second.Stale {
		t.Fatal("offline query did not report stale cache")
	}
	if len(second.Models) != 1 || second.Models[0].ID != "cached-model" {
		t.Fatalf("stale models = %#v", second.Models)
	}
}

func TestFetchOpenAIModelsUsesResolvedRuntimeAPIKey(t *testing.T) {
	oldClient := modelListHTTPClient
	t.Cleanup(func() { modelListHTTPClient = oldClient })
	oldModelsDevFetcher := modelsDevFetcher
	t.Cleanup(func() { modelsDevFetcher = oldModelsDevFetcher })
	modelsDevFetcher = func(context.Context) (map[string]int64, error) {
		return map[string]int64{}, nil
	}
	modelListHTTPClient = &http.Client{Transport: roundTripperFunc(func(req *http.Request) (*http.Response, error) {
		if got, want := req.URL.String(), "https://api.openai.com/v1/models"; got != want {
			t.Fatalf("catalog URL = %q, want %q", got, want)
		}
		if got, want := req.Header.Get("Authorization"), "Bearer runtime-key"; got != want {
			t.Fatalf("authorization = %q, want %q", got, want)
		}
		return &http.Response{
			StatusCode: http.StatusOK,
			Body:       io.NopCloser(strings.NewReader(`{"data":[{"id":"gpt-runtime","created":123}]}`)),
			Header:     make(http.Header),
		}, nil
	})}

	models, err := fetchModels(t.Context(), "openai", &config.Config{
		Provider:               "openai",
		APIKeyOverride:         "runtime-key",
		APIKeyOverrideProvider: "openai",
	})
	if err != nil {
		t.Fatalf("fetch openai models: %v", err)
	}
	if len(models) != 1 || models[0].ID != "gpt-runtime" || models[0].Name != "gpt-runtime" {
		t.Fatalf("models = %#v", models)
	}
}

func TestFetchOpenAICompatibleModelsUsesCustomAuthEnvEndpointAndHeaders(t *testing.T) {
	t.Setenv("ION_TEST_CATALOG_KEY", "env-key")
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/models" {
			t.Fatalf("path = %q, want /v1/models", r.URL.Path)
		}
		if got, want := r.Header.Get("Authorization"), "Bearer env-key"; got != want {
			t.Fatalf("authorization = %q, want %q", got, want)
		}
		if got, want := r.Header.Get("X-Catalog-Test"), "present"; got != want {
			t.Fatalf("custom header = %q, want %q", got, want)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"data":[{"id":"custom-model"}]}`)
	}))
	defer server.Close()

	models, err := fetchModels(t.Context(), OpenAICompatibleID, &config.Config{
		Provider:   OpenAICompatibleID,
		Endpoint:   server.URL + "/v1",
		AuthEnvVar: "ION_TEST_CATALOG_KEY",
		ExtraHeaders: map[string]string{
			"X-Catalog-Test": "present",
		},
	})
	if err != nil {
		t.Fatalf("fetch custom models: %v", err)
	}
	if len(models) != 1 || models[0].ID != "custom-model" {
		t.Fatalf("models = %#v", models)
	}
}

func TestQueryAvailableModelsFiltersUnconfiguredProviders(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "configured")
	t.Setenv("OPENROUTER_API_KEY", "")
	providerModelsOnce = sync.Once{}
	providerModelsCacheMap = nil

	oldFetcher := providerCatalogFetcher
	t.Cleanup(func() { providerCatalogFetcher = oldFetcher })
	providerCatalogFetcher = func(ctx context.Context, provider string, cfg *config.Config) ([]ModelMetadata, error) {
		return []ModelMetadata{{ID: provider + "-model", Provider: provider}}, nil
	}

	result, err := QueryAvailableModels(t.Context(), ModelCatalogQuery{
		Providers: []string{"openai", "openrouter"},
	})
	if err != nil {
		t.Fatalf("query available models: %v", err)
	}
	if len(result.Models) != 1 || result.Models[0].Provider != "openai" {
		t.Fatalf("models = %#v, want only configured openai model", result.Models)
	}
	if len(result.Status) != 1 || result.Status[0].Provider != "openai" {
		t.Fatalf("status = %#v, want only configured provider", result.Status)
	}
}

type roundTripperFunc func(*http.Request) (*http.Response, error)

func (f roundTripperFunc) RoundTrip(req *http.Request) (*http.Response, error) {
	return f(req)
}

func TestListModelsForConfigRejectsNilConfig(t *testing.T) {
	_, err := ListModelsForConfig(t.Context(), nil)
	if err == nil || err.Error() != "model provider config is required" {
		t.Fatalf("ListModelsForConfig(nil) error = %v", err)
	}
}

func TestListModelsForConfigWrapsDeadlineWithoutRawContextText(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	providerModelsOnce = sync.Once{}
	providerModelsCacheMap = nil

	oldFetcher := providerCatalogFetcher
	t.Cleanup(func() { providerCatalogFetcher = oldFetcher })
	providerCatalogFetcher = func(ctx context.Context, provider string, cfg *config.Config) ([]ModelMetadata, error) {
		return nil, context.DeadlineExceeded
	}

	_, err := ListModelsForConfig(t.Context(), &config.Config{Provider: "openrouter"})
	if err == nil {
		t.Fatal("ListModelsForConfig returned nil error")
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("error = %v, want context deadline cause", err)
	}
	if got := err.Error(); got != "list models for openrouter timed out after 10s" {
		t.Fatalf("model timeout error = %q", got)
	}
	if strings.Contains(err.Error(), "context deadline exceeded") {
		t.Fatalf("model timeout leaked raw context text: %q", err.Error())
	}
}

func TestModelCacheTTLUsesShortTTLForLocalCatalogs(t *testing.T) {
	shortTTLConfigs := []*config.Config{
		{Provider: "local-api"},
		{Provider: "ollama"},
		{Provider: "openai-compatible", Endpoint: "http://127.0.0.1:8080/v1"},
		{Provider: "openai-compatible", Endpoint: "http://localhost:8080/v1"},
	}
	for _, cfg := range shortTTLConfigs {
		if got := modelCacheTTL(cfg); got != localModelCacheTTL {
			t.Fatalf("modelCacheTTL(%#v) = %v, want %v", cfg, got, localModelCacheTTL)
		}
	}

	remoteTTL := time.Duration(config.DefaultModelCacheTTLSeconds()) * time.Second
	if got := modelCacheTTL(&config.Config{Provider: "openrouter"}); got != remoteTTL {
		t.Fatalf("remote modelCacheTTL = %v, want %v", got, remoteTTL)
	}
}

func TestCachedFreshForConfigExpiresLocalCatalogsSooner(t *testing.T) {
	updatedAt := time.Now().Add(-30 * time.Second).Unix()
	if cachedFreshForConfig(updatedAt, &config.Config{Provider: "local-api"}) {
		t.Fatal("local-api cache stayed fresh past local TTL")
	}
	if !cachedFreshForConfig(updatedAt, &config.Config{Provider: "openrouter"}) {
		t.Fatal("remote catalog cache expired before default TTL")
	}
}

func TestFetchModelsUsesDirectFetcherForNativeProviders(t *testing.T) {
	tests := []struct {
		provider string
		target   *providerModelFetcher
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
			*tc.target = func(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
				called = true
				return []ModelMetadata{{ID: tc.provider + "-model"}}, nil
			}
			defer func() { *tc.target = original }()

			models, err := fetchModels(
				context.Background(),
				tc.provider,
				&config.Config{Provider: tc.provider},
			)
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
