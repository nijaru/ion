package registry

import (
	"context"
	"os"
	"path/filepath"
	"sync"
	"testing"
)

func TestListModelsCachesProviderModels(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	providerModelsOnce = sync.Once{}
	providerModelsCacheMap = nil

	oldFetcher := providerCatalogFetcher
	defer func() { providerCatalogFetcher = oldFetcher }()

	var calls int
	providerCatalogFetcher = func(ctx context.Context, provider string) ([]ModelMetadata, error) {
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
