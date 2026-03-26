package registry

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"charm.land/catwalk/pkg/catwalk"
)

type ModelMetadata struct {
	ID           string  `json:"id"`
	Provider     string  `json:"provider"`
	ContextLimit int     `json:"context_limit"`
	InputPrice   float64 `json:"input_price"`  // per 1M tokens
	OutputPrice  float64 `json:"output_price"` // per 1M tokens
	UpdatedAt    int64   `json:"updated_at"`
}

var (
	registryCache map[string]ModelMetadata
	registryMu    sync.RWMutex
	registryOnce  sync.Once
)

func initRegistry() {
	registryMu.Lock()
	defer registryMu.Unlock()
	registryCache = make(map[string]ModelMetadata)
	loadCache()
}

func GetMetadata(ctx context.Context, provider, model string) (ModelMetadata, bool) {
	registryOnce.Do(initRegistry)

	key := fmt.Sprintf("%s/%s", strings.ToLower(provider), strings.ToLower(model))

	registryMu.RLock()
	meta, ok := registryCache[key]
	registryMu.RUnlock()

	// If found and fresh enough (e.g. 24h), return it
	if ok && time.Now().Unix()-meta.UpdatedAt < 86400 {
		return meta, true
	}

	// Try fetching from catwalk
	fetched, err := fetchFromCatwalk(ctx, provider, model)
	if err == nil {
		registryMu.Lock()
		registryCache[key] = fetched
		saveCache()
		registryMu.Unlock()
		return fetched, true
	}

	// Fallback to cache even if stale
	if ok {
		return meta, true
	}

	// Fallback to built-in defaults
	if meta, ok := builtInMetadata[model]; ok {
		return meta, true
	}
	if meta, ok := builtInMetadata[strings.ToLower(provider)]; ok {
		return meta, true
	}

	return ModelMetadata{}, false
}

func fetchFromCatwalk(ctx context.Context, provider, model string) (ModelMetadata, error) {
	client := catwalk.New()
	providers, err := client.GetProviders(ctx, "")
	if err != nil {
		return ModelMetadata{}, err
	}

	for _, p := range providers {
		// Try to match provider by ID or Name
		if strings.EqualFold(p.Name, provider) || strings.EqualFold(string(p.ID), provider) {
			for _, m := range p.Models {
				// Try to match model by ID or Name
				if strings.EqualFold(m.ID, model) || strings.EqualFold(m.Name, model) {
					return ModelMetadata{
						ID:           m.ID,
						Provider:     p.Name,
						ContextLimit: int(m.ContextWindow),
						InputPrice:   m.CostPer1MIn,
						OutputPrice:  m.CostPer1MOut,
						UpdatedAt:    time.Now().Unix(),
					}, nil
				}
			}
		}
	}
	return ModelMetadata{}, fmt.Errorf("model %s not found for provider %s", model, provider)
}

func cachePath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".ion", "data", "metadata_cache.json")
}

func loadCache() {
	data, err := os.ReadFile(cachePath())
	if err != nil {
		return
	}
	_ = json.Unmarshal(data, &registryCache)
}

func saveCache() {
	data, err := json.MarshalIndent(registryCache, "", "  ")
	if err != nil {
		return
	}
	path := cachePath()
	_ = os.MkdirAll(filepath.Dir(path), 0755)
	_ = os.WriteFile(path, data, 0644)
}

var builtInMetadata = map[string]ModelMetadata{
	"gemini": {
		ContextLimit: 1_000_000,
	},
	"claude": {
		ContextLimit: 200_000,
	},
	"gpt-4": {
		ContextLimit: 128_000,
	},
	"minimax-m2.7": {
		ContextLimit: 200_000,
	},
}
