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

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
)

type ModelMetadata struct {
	ID               string  `json:"id"`
	Provider         string  `json:"provider"`
	ContextLimit     int     `json:"context_limit"`
	InputPrice       float64 `json:"input_price"`  // per 1M tokens
	OutputPrice      float64 `json:"output_price"` // per 1M tokens
	InputPriceKnown  bool    `json:"input_price_known"`
	OutputPriceKnown bool    `json:"output_price_known"`
	Created          int64   `json:"created"`
	UpdatedAt        int64   `json:"updated_at"`
}

var (
	registryCache   map[string]ModelMetadata
	registryMu      sync.RWMutex
	registryOnce    sync.Once
	metadataFetcher = fetchMetadata
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

	fetched, err := metadataFetcher(ctx, provider, model)
	if err == nil {
		registryMu.Lock()
		registryCache[key] = fetched
		saveCache()
		registryMu.Unlock()
		return fetched, true
	}

	return ModelMetadata{}, false
}

func fetchMetadata(ctx context.Context, provider, model string) (ModelMetadata, error) {
	if def, ok := providers.Lookup(provider); ok && def.Runtime == providers.RuntimeNative {
		models, err := ListModelsForConfig(ctx, &config.Config{Provider: provider})
		if err != nil {
			return ModelMetadata{}, err
		}
		for _, meta := range models {
			if strings.EqualFold(meta.ID, model) {
				return meta, nil
			}
		}
		return ModelMetadata{}, fmt.Errorf("model %s not found for provider %s", model, provider)
	}
	return ModelMetadata{}, fmt.Errorf("no live metadata catalog configured for provider %s", provider)
}

func cachePath() string {
	dataDir, err := config.DefaultDataDir()
	if err != nil {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, ".ion", "data", "metadata_cache.json")
	}
	return filepath.Join(dataDir, "metadata_cache.json")
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
