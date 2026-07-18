package llm

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
)

func TestGetMetadataUsesInjectedFetcher(t *testing.T) {
	catalog := NewModelCatalog(ModelCatalogOptions{DataDir: t.TempDir()})
	catalog.metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		return ModelMetadata{
			ID:           model,
			Provider:     provider,
			ContextLimit: 123000,
			InputPrice:   0.1,
			OutputPrice:  0.2,
			UpdatedAt:    1,
		}, nil
	}

	meta, ok := catalog.GetMetadata(context.Background(), "openrouter", "openai/gpt-5.4")
	if !ok {
		t.Fatal("expected metadata fetch to succeed")
	}
	if meta.Provider != "openrouter" || meta.ID != "openai/gpt-5.4" {
		t.Fatalf("unexpected metadata: %#v", meta)
	}
}

func TestGetCachedMetadataDoesNotFetch(t *testing.T) {
	catalog := NewModelCatalog(ModelCatalogOptions{DataDir: t.TempDir()})

	var calls int
	catalog.metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		calls++
		return ModelMetadata{
			ID:        model,
			Provider:  provider,
			UpdatedAt: time.Now().Unix(),
		}, nil
	}

	if meta, ok := catalog.GetCachedMetadata("openrouter", "openai/gpt-5.4"); ok {
		t.Fatalf("cached metadata = %#v, want miss", meta)
	}
	if calls != 0 {
		t.Fatalf("metadata fetch calls = %d, want 0", calls)
	}
}

func TestCachedContextLimitUsesOnlyRegistryCache(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	catalog := NewModelCatalog(ModelCatalogOptions{DataDir: t.TempDir()})

	var calls int
	catalog.metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		calls++
		return ModelMetadata{
			ID:           model,
			Provider:     provider,
			ContextLimit: 123000,
			UpdatedAt:    time.Now().Unix(),
		}, nil
	}

	if limit, ok := catalog.CachedContextLimit("openrouter", "openai/gpt-5.4"); ok {
		t.Fatalf("cached context limit = %d, want cache miss", limit)
	}
	if calls != 0 {
		t.Fatalf("metadata fetch calls = %d, want 0", calls)
	}

	catalog.metadataMu.Lock()
	catalog.metadataCache[metadataKey("openrouter", "openai/gpt-5.4")] = ModelMetadata{
		ID:           "openai/gpt-5.4",
		Provider:     "openrouter",
		ContextLimit: 456000,
		UpdatedAt:    time.Now().Unix(),
	}
	catalog.metadataMu.Unlock()

	limit, ok := catalog.CachedContextLimit("openrouter", "openai/gpt-5.4")
	if !ok {
		t.Fatal("expected cached context limit")
	}
	if limit != 456000 {
		t.Fatalf("context limit = %d, want 456000", limit)
	}
	if calls != 0 {
		t.Fatalf("metadata fetch calls after cache hit = %d, want 0", calls)
	}
}

func TestCachedContextLimitForConfigUsesProviderModelCache(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	catalog := NewModelCatalog(ModelCatalogOptions{DataDir: t.TempDir()})

	var metadataCalls int
	var catalogCalls int
	catalog.metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		metadataCalls++
		return ModelMetadata{
			ID:           model,
			Provider:     provider,
			ContextLimit: 123000,
			UpdatedAt:    time.Now().Unix(),
		}, nil
	}
	catalog.providerCatalogFetcher = func(ctx context.Context, provider string, cfg *config.Config) ([]ModelMetadata, error) {
		catalogCalls++
		return []ModelMetadata{{ID: "vendor/model", ContextLimit: 123000}}, nil
	}

	cfg := &config.Config{Provider: "openrouter", Model: "vendor/model"}
	if limit, ok := catalog.CachedContextLimitForConfig(cfg); ok {
		t.Fatalf("cached context limit = %d, want cache miss", limit)
	}
	if metadataCalls != 0 || catalogCalls != 0 {
		t.Fatalf(
			"fetch calls = metadata %d/catalog %d, want zero",
			metadataCalls,
			catalogCalls,
		)
	}

	catalog.providerModelsMu.Lock()
	catalog.providerModelsCacheMap[providerCacheKey(cfg)] = providerModelsCache{
		UpdatedAt: time.Now().Unix(),
		Models: []ModelMetadata{{
			ID:           "vendor/model",
			Provider:     "openrouter",
			ContextLimit: 456000,
		}},
	}
	catalog.providerModelsMu.Unlock()

	limit, ok := catalog.CachedContextLimitForConfig(cfg)
	if !ok {
		t.Fatal("expected cached provider model context limit")
	}
	if limit != 456000 {
		t.Fatalf("context limit = %d, want 456000", limit)
	}
	if metadataCalls != 0 || catalogCalls != 0 {
		t.Fatalf(
			"fetch calls after provider cache hit = metadata %d/catalog %d, want zero",
			metadataCalls,
			catalogCalls,
		)
	}
}
