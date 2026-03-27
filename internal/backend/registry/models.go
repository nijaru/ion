package registry

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"charm.land/catwalk/pkg/catwalk"
	"github.com/nijaru/ion/internal/config"
)

type providerModelsCache struct {
	UpdatedAt int64           `json:"updated_at"`
	Models    []ModelMetadata `json:"models"`
}

var (
	providerModelsMu       sync.RWMutex
	providerModelsOnce     sync.Once
	providerModelsCacheMap map[string]providerModelsCache
	providerCatalogFetcher = fetchModelsFromCatwalk
)

func ListModels(ctx context.Context, provider string) ([]ModelMetadata, error) {
	providerModelsOnce.Do(initProviderModelsCache)

	key := strings.ToLower(strings.TrimSpace(provider))
	providerModelsMu.RLock()
	cached, ok := providerModelsCacheMap[key]
	providerModelsMu.RUnlock()
	if ok && cachedFresh(cached.UpdatedAt) {
		return append([]ModelMetadata(nil), cached.Models...), nil
	}

	fetched, err := providerCatalogFetcher(ctx, provider)
	if err == nil {
		slices.SortFunc(fetched, func(a, b ModelMetadata) int {
			return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
		})
		providerModelsMu.Lock()
		providerModelsCacheMap[key] = providerModelsCache{
			UpdatedAt: time.Now().Unix(),
			Models:    append([]ModelMetadata(nil), fetched...),
		}
		saveProviderModelsCache()
		providerModelsMu.Unlock()
		return fetched, nil
	}

	if ok {
		return append([]ModelMetadata(nil), cached.Models...), nil
	}

	return nil, err
}

func initProviderModelsCache() {
	providerModelsMu.Lock()
	defer providerModelsMu.Unlock()
	providerModelsCacheMap = make(map[string]providerModelsCache)
	loadProviderModelsCache()
}

func fetchModelsFromCatwalk(ctx context.Context, provider string) ([]ModelMetadata, error) {
	client := catwalk.New()
	providers, err := client.GetProviders(ctx, "")
	if err != nil {
		return nil, err
	}

	var models []ModelMetadata
	for _, p := range providers {
		if !strings.EqualFold(p.Name, provider) && !strings.EqualFold(string(p.ID), provider) {
			continue
		}
		for _, m := range p.Models {
			models = append(models, ModelMetadata{
				ID:           m.ID,
				Provider:     p.Name,
				ContextLimit: int(m.ContextWindow),
				InputPrice:   m.CostPer1MIn,
				OutputPrice:  m.CostPer1MOut,
				UpdatedAt:    time.Now().Unix(),
			})
		}
		break
	}

	if len(models) == 0 {
		return nil, fmt.Errorf("no models found for provider %s", provider)
	}

	slices.SortFunc(models, func(a, b ModelMetadata) int {
		return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
	})
	return models, nil
}

func cachedFresh(updatedAt int64) bool {
	if updatedAt <= 0 {
		return false
	}
	return time.Since(time.Unix(updatedAt, 0)) < time.Duration(config.DefaultModelCacheTTLSeconds())*time.Second
}

func providerModelsCachePath() string {
	dataDir, err := config.DefaultDataDir()
	if err != nil {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, ".ion", "data", "models_cache.json")
	}
	return filepath.Join(dataDir, "models_cache.json")
}

func loadProviderModelsCache() {
	data, err := os.ReadFile(providerModelsCachePath())
	if err != nil {
		return
	}
	_ = json.Unmarshal(data, &providerModelsCacheMap)
}

func saveProviderModelsCache() {
	data, err := json.MarshalIndent(providerModelsCacheMap, "", "  ")
	if err != nil {
		return
	}
	path := providerModelsCachePath()
	_ = os.MkdirAll(filepath.Dir(path), 0o755)
	_ = os.WriteFile(path, data, 0o644)
}
