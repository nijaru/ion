package registry

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"charm.land/catwalk/pkg/catwalk"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
)

type providerModelsCache struct {
	UpdatedAt int64           `json:"updated_at"`
	Models    []ModelMetadata `json:"models"`
}

var (
	providerModelsMu       sync.RWMutex
	providerModelsOnce     sync.Once
	providerModelsCacheMap map[string]providerModelsCache
	providerCatalogFetcher = fetchModels
	openAIFetcher          = fetchOpenAIModels
	anthropicFetcher       = fetchAnthropicModels
	openRouterFetcher      = fetchOpenRouterModels
	geminiFetcher          = fetchGeminiModels
	ollamaFetcher          = fetchOllamaModels
	catwalkFetcher         = fetchModelsFromCatwalk
)

func ListModels(ctx context.Context, provider string) ([]ModelMetadata, error) {
	return ListModelsForConfig(ctx, &config.Config{Provider: provider})
}

func ListModelsForConfig(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
	providerModelsOnce.Do(initProviderModelsCache)

	key := providerCacheKey(cfg)
	providerModelsMu.RLock()
	cached, ok := providerModelsCacheMap[key]
	providerModelsMu.RUnlock()
	if ok && cachedFresh(cached.UpdatedAt) {
		return append([]ModelMetadata(nil), cached.Models...), nil
	}

	fetched, err := providerCatalogFetcher(ctx, cfg.Provider, cfg)
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

func fetchModels(ctx context.Context, provider string, cfg *config.Config) ([]ModelMetadata, error) {
	provider = providers.ResolveID(provider)
	switch provider {
	case "anthropic":
		return anthropicFetcher(ctx)
	case "openai":
		return openAIFetcher(ctx)
	case "openrouter":
		return openRouterFetcher(ctx)
	case "gemini":
		return geminiFetcher(ctx)
	case "ollama":
		if endpoint := providers.ResolvedEndpoint(cfg); endpoint != "" && endpoint != "http://localhost:11434/v1" {
			return fetchOpenAICompatibleModels(ctx, provider, providers.ResolvedEndpoint(cfg), "", nil)
		}
		return ollamaFetcher(ctx)
	default:
		if def, ok := providers.Lookup(provider); ok && def.Family == providers.FamilyOpenAI {
			endpoint := providers.ResolvedEndpoint(cfg)
			if endpoint == "" {
				return nil, fmt.Errorf("provider %s has no configured endpoint", provider)
			}
			return fetchOpenAICompatibleModels(ctx, provider, endpoint, resolvedAuthToken(cfg, def), providers.ResolvedHeaders(cfg))
		}
		if strings.TrimSpace(os.Getenv("CATWALK_URL")) == "" {
			return nil, fmt.Errorf("no live model catalog configured for provider %s", provider)
		}
		return catwalkFetcher(ctx, provider)
	}
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

type openRouterModelsResponse struct {
	Data []openRouterModel `json:"data"`
}

type openAIModelsResponse struct {
	Data []openAIModel `json:"data"`
}

type openAIModel struct {
	ID string `json:"id"`
}

type anthropicModelsResponse struct {
	Data []anthropicModel `json:"data"`
}

type anthropicModel struct {
	ID             string `json:"id"`
	DisplayName    string `json:"display_name"`
	MaxInputTokens int    `json:"max_input_tokens"`
}

type openRouterModel struct {
	ID            string             `json:"id"`
	Name          string             `json:"name"`
	ContextLength int64              `json:"context_length"`
	Pricing       openRouterPricing  `json:"pricing"`
	TopProvider   openRouterProvider `json:"top_provider"`
}

type openRouterPricing struct {
	Prompt     string `json:"prompt"`
	Completion string `json:"completion"`
}

type openRouterProvider struct {
	ContextLength int64 `json:"context_length"`
}

type geminiModelsResponse struct {
	Models        []geminiModel `json:"models"`
	NextPageToken string        `json:"nextPageToken"`
}

type geminiModel struct {
	Name                       string   `json:"name"`
	BaseModelID                string   `json:"baseModelId"`
	InputTokenLimit            int      `json:"inputTokenLimit"`
	SupportedGenerationMethods []string `json:"supportedGenerationMethods"`
}

type ollamaTagsResponse struct {
	Models []ollamaModel `json:"models"`
}

type ollamaModel struct {
	Name string `json:"name"`
}

func fetchOpenAIModels(ctx context.Context) ([]ModelMetadata, error) {
	apiKey := strings.TrimSpace(os.Getenv("OPENAI_API_KEY"))
	if apiKey == "" {
		return nil, fmt.Errorf("OPENAI_API_KEY not set")
	}

	var payload openAIModelsResponse
	if err := fetchJSON(ctx, http.MethodGet, "https://api.openai.com/v1/models", map[string]string{
		"Authorization": "Bearer " + apiKey,
	}, &payload); err != nil {
		return nil, fmt.Errorf("fetch openai models: %w", err)
	}

	models := make([]ModelMetadata, 0, len(payload.Data))
	for _, model := range payload.Data {
		id := strings.TrimSpace(model.ID)
		if id == "" {
			continue
		}
		models = append(models, ModelMetadata{
			ID:        id,
			Provider:  "openai",
			UpdatedAt: time.Now().Unix(),
		})
	}
	return sortModels(models), nil
}

func fetchAnthropicModels(ctx context.Context) ([]ModelMetadata, error) {
	apiKey := strings.TrimSpace(os.Getenv("ANTHROPIC_API_KEY"))
	if apiKey == "" {
		return nil, fmt.Errorf("ANTHROPIC_API_KEY not set")
	}

	var payload anthropicModelsResponse
	if err := fetchJSON(ctx, http.MethodGet, "https://api.anthropic.com/v1/models?limit=1000", map[string]string{
		"X-Api-Key":         apiKey,
		"anthropic-version": "2023-06-01",
	}, &payload); err != nil {
		return nil, fmt.Errorf("fetch anthropic models: %w", err)
	}

	models := make([]ModelMetadata, 0, len(payload.Data))
	for _, model := range payload.Data {
		id := strings.TrimSpace(model.ID)
		if id == "" {
			continue
		}
		models = append(models, ModelMetadata{
			ID:           id,
			Provider:     "anthropic",
			ContextLimit: model.MaxInputTokens,
			UpdatedAt:    time.Now().Unix(),
		})
	}
	return sortModels(models), nil
}

func fetchOpenRouterModels(ctx context.Context) ([]ModelMetadata, error) {
	var payload openRouterModelsResponse
	if err := fetchJSON(ctx, http.MethodGet, "https://openrouter.ai/api/v1/models", nil, &payload); err != nil {
		return nil, fmt.Errorf("fetch openrouter models: %w", err)
	}

	models := make([]ModelMetadata, 0, len(payload.Data))
	for _, model := range payload.Data {
		contextLimit := int(model.ContextLength)
		if contextLimit == 0 {
			contextLimit = int(model.TopProvider.ContextLength)
		}
		models = append(models, ModelMetadata{
			ID:           model.ID,
			Provider:     "openrouter",
			ContextLimit: contextLimit,
			InputPrice:   parseMillionCost(model.Pricing.Prompt),
			OutputPrice:  parseMillionCost(model.Pricing.Completion),
			UpdatedAt:    time.Now().Unix(),
		})
	}

	return sortModels(models), nil
}

func fetchGeminiModels(ctx context.Context) ([]ModelMetadata, error) {
	apiKey := strings.TrimSpace(os.Getenv("GEMINI_API_KEY"))
	if apiKey == "" {
		apiKey = strings.TrimSpace(os.Getenv("GOOGLE_API_KEY"))
	}
	if apiKey == "" {
		return nil, fmt.Errorf("GEMINI_API_KEY or GOOGLE_API_KEY not set")
	}

	base := "https://generativelanguage.googleapis.com/v1beta/models"
	models := make([]ModelMetadata, 0, 128)
	pageToken := ""
	for {
		endpoint := base + "?key=" + url.QueryEscape(apiKey) + "&pageSize=1000"
		if pageToken != "" {
			endpoint += "&pageToken=" + url.QueryEscape(pageToken)
		}

		var payload geminiModelsResponse
		if err := fetchJSON(ctx, http.MethodGet, endpoint, nil, &payload); err != nil {
			return nil, fmt.Errorf("fetch gemini models: %w", err)
		}

		for _, model := range payload.Models {
			if !supportsGenerationMethod(model.SupportedGenerationMethods, "generateContent") {
				continue
			}
			id := strings.TrimSpace(strings.TrimPrefix(model.Name, "models/"))
			if id == "" {
				id = strings.TrimSpace(model.BaseModelID)
			}
			if id == "" {
				continue
			}
			models = append(models, ModelMetadata{
				ID:           id,
				Provider:     "gemini",
				ContextLimit: model.InputTokenLimit,
				UpdatedAt:    time.Now().Unix(),
			})
		}

		if strings.TrimSpace(payload.NextPageToken) == "" {
			break
		}
		pageToken = payload.NextPageToken
	}

	return sortModels(models), nil
}

func fetchOllamaModels(ctx context.Context) ([]ModelMetadata, error) {
	base := normalizeOllamaBaseURL(strings.TrimSpace(os.Getenv("OLLAMA_HOST")))
	var payload ollamaTagsResponse
	if err := fetchJSON(ctx, http.MethodGet, base+"/api/tags", nil, &payload); err != nil {
		return nil, fmt.Errorf("fetch ollama models: %w", err)
	}

	models := make([]ModelMetadata, 0, len(payload.Models))
	for _, model := range payload.Models {
		id := strings.TrimSpace(model.Name)
		if id == "" {
			continue
		}
		models = append(models, ModelMetadata{
			ID:        id,
			Provider:  "ollama",
			UpdatedAt: time.Now().Unix(),
		})
	}
	return sortModels(models), nil
}

func fetchOpenAICompatibleModels(ctx context.Context, provider, endpoint, token string, extraHeaders map[string]string) ([]ModelMetadata, error) {
	headers := make(map[string]string, len(extraHeaders)+1)
	for k, v := range extraHeaders {
		headers[k] = v
	}
	if strings.TrimSpace(token) != "" {
		headers["Authorization"] = "Bearer " + token
	}

	var payload openAIModelsResponse
	if err := fetchJSON(ctx, http.MethodGet, strings.TrimRight(endpoint, "/")+"/models", headers, &payload); err != nil {
		return nil, fmt.Errorf("fetch %s models: %w", provider, err)
	}

	models := make([]ModelMetadata, 0, len(payload.Data))
	for _, model := range payload.Data {
		id := strings.TrimSpace(model.ID)
		if id == "" {
			continue
		}
		models = append(models, ModelMetadata{
			ID:        id,
			Provider:  provider,
			UpdatedAt: time.Now().Unix(),
		})
	}
	return sortModels(models), nil
}

func parseMillionCost(raw string) float64 {
	value, err := strconv.ParseFloat(raw, 64)
	if err != nil {
		return 0
	}
	return value * 1_000_000
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

func fetchJSON(ctx context.Context, method, endpoint string, headers map[string]string, into any) error {
	req, err := http.NewRequestWithContext(ctx, method, endpoint, nil)
	if err != nil {
		return fmt.Errorf("build request: %w", err)
	}
	req.Header.Set("User-Agent", "ion/0.0.0")
	for key, value := range headers {
		req.Header.Set(key, value)
	}

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("read response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("%s returned %d: %s", endpoint, resp.StatusCode, strings.TrimSpace(string(body)))
	}
	if err := json.Unmarshal(body, into); err != nil {
		return fmt.Errorf("decode response: %w", err)
	}
	return nil
}

func sortModels(models []ModelMetadata) []ModelMetadata {
	slices.SortFunc(models, func(a, b ModelMetadata) int {
		return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
	})
	return models
}

func supportsGenerationMethod(methods []string, want string) bool {
	for _, method := range methods {
		if strings.EqualFold(strings.TrimSpace(method), want) {
			return true
		}
	}
	return false
}

func normalizeOllamaBaseURL(raw string) string {
	base := strings.TrimSpace(raw)
	if base == "" {
		return "http://127.0.0.1:11434"
	}
	if strings.Contains(base, "://") {
		return strings.TrimRight(base, "/")
	}
	return "http://" + strings.TrimRight(base, "/")
}

func providerCacheKey(cfg *config.Config) string {
	if cfg == nil {
		return ""
	}
	provider := providers.ResolveID(cfg.Provider)
	endpoint := providers.ResolvedEndpoint(cfg)
	authEnv := providers.ResolvedAuthEnvVar(cfg)
	return strings.Join([]string{
		provider,
		strings.ToLower(strings.TrimSpace(endpoint)),
		strings.TrimSpace(authEnv),
	}, "|")
}

func resolvedAuthToken(cfg *config.Config, def providers.Definition) string {
	names := make([]string, 0, 1+len(def.AlternateEnvVars))
	if override := strings.TrimSpace(cfg.AuthEnvVar); override != "" {
		names = append(names, override)
	}
	if def.DefaultEnvVar != "" {
		names = append(names, def.DefaultEnvVar)
	}
	names = append(names, def.AlternateEnvVars...)
	for _, name := range names {
		if value := strings.TrimSpace(os.Getenv(name)); value != "" {
			return value
		}
	}
	return ""
}
