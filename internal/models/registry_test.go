package models

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/config"
)

func TestGetMetadataUsesInjectedFetcher(t *testing.T) {
	registryOnce = sync.Once{}
	registryCache = nil

	oldFetcher := metadataFetcher
	defer func() { metadataFetcher = oldFetcher }()

	metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		return ModelMetadata{
			ID:           model,
			Provider:     provider,
			ContextLimit: 123000,
			InputPrice:   0.1,
			OutputPrice:  0.2,
			UpdatedAt:    1,
		}, nil
	}

	meta, ok := GetMetadata(context.Background(), "openrouter", "openai/gpt-5.4")
	if !ok {
		t.Fatal("expected metadata fetch to succeed")
	}
	if meta.Provider != "openrouter" || meta.ID != "openai/gpt-5.4" {
		t.Fatalf("unexpected metadata: %#v", meta)
	}
}

func TestGetCachedMetadataDoesNotFetch(t *testing.T) {
	registryOnce = sync.Once{}
	registryCache = nil

	oldFetcher := metadataFetcher
	defer func() { metadataFetcher = oldFetcher }()

	var calls int
	metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		calls++
		return ModelMetadata{
			ID:        model,
			Provider:  provider,
			UpdatedAt: time.Now().Unix(),
		}, nil
	}

	if meta, ok := GetCachedMetadata("openrouter", "openai/gpt-5.4"); ok {
		t.Fatalf("cached metadata = %#v, want miss", meta)
	}
	if calls != 0 {
		t.Fatalf("metadata fetch calls = %d, want 0", calls)
	}
}

func TestCachedContextLimitUsesOnlyRegistryCache(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	registryOnce = sync.Once{}
	registryCache = nil

	oldFetcher := metadataFetcher
	defer func() { metadataFetcher = oldFetcher }()

	var calls int
	metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		calls++
		return ModelMetadata{
			ID:           model,
			Provider:     provider,
			ContextLimit: 123000,
			UpdatedAt:    time.Now().Unix(),
		}, nil
	}

	if limit, ok := CachedContextLimit("openrouter", "openai/gpt-5.4"); ok {
		t.Fatalf("cached context limit = %d, want cache miss", limit)
	}
	if calls != 0 {
		t.Fatalf("metadata fetch calls = %d, want 0", calls)
	}

	registryMu.Lock()
	registryCache[metadataKey("openrouter", "openai/gpt-5.4")] = ModelMetadata{
		ID:           "openai/gpt-5.4",
		Provider:     "openrouter",
		ContextLimit: 456000,
		UpdatedAt:    time.Now().Unix(),
	}
	registryMu.Unlock()

	limit, ok := CachedContextLimit("openrouter", "openai/gpt-5.4")
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
	registryOnce = sync.Once{}
	registryCache = nil
	providerModelsOnce = sync.Once{}
	providerModelsCacheMap = nil

	oldMetadataFetcher := metadataFetcher
	oldCatalogFetcher := providerCatalogFetcher
	defer func() {
		metadataFetcher = oldMetadataFetcher
		providerCatalogFetcher = oldCatalogFetcher
	}()

	var metadataCalls int
	var catalogCalls int
	metadataFetcher = func(ctx context.Context, provider, model string) (ModelMetadata, error) {
		metadataCalls++
		return ModelMetadata{
			ID:           model,
			Provider:     provider,
			ContextLimit: 123000,
			UpdatedAt:    time.Now().Unix(),
		}, nil
	}
	providerCatalogFetcher = func(ctx context.Context, provider string, cfg *config.Config) ([]ModelMetadata, error) {
		catalogCalls++
		return []ModelMetadata{{ID: "vendor/model", ContextLimit: 123000}}, nil
	}

	cfg := &config.Config{Provider: "openrouter", Model: "vendor/model"}
	if limit, ok := CachedContextLimitForConfig(cfg); ok {
		t.Fatalf("cached context limit = %d, want cache miss", limit)
	}
	if metadataCalls != 0 || catalogCalls != 0 {
		t.Fatalf(
			"fetch calls = metadata %d/catalog %d, want zero",
			metadataCalls,
			catalogCalls,
		)
	}

	providerModelsOnce.Do(initProviderModelsCache)
	providerModelsMu.Lock()
	providerModelsCacheMap[providerCacheKey(cfg)] = providerModelsCache{
		UpdatedAt: time.Now().Unix(),
		Models: []ModelMetadata{{
			ID:           "vendor/model",
			Provider:     "openrouter",
			ContextLimit: 456000,
		}},
	}
	providerModelsMu.Unlock()

	limit, ok := CachedContextLimitForConfig(cfg)
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
