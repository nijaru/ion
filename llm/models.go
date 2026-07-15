package llm

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
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

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/ctxerr"
)

type providerModelsCache struct {
	UpdatedAt int64           `json:"updated_at"`
	Models    []ModelMetadata `json:"models"`
}

// ModelListResult describes the source used for a provider model query.
// Stale is true when the provider could not be reached and the result came
// from the persisted cache. Callers must surface that state when freshness
// matters; the cache is a resilience mechanism, not a silent success.
type ModelListResult struct {
	Models    []ModelMetadata
	UpdatedAt time.Time
	Stale     bool
}

// ModelCatalogQuery selects providers for an aggregate available-model query.
// An empty Providers list means all native providers with model-list support.
// Config supplies the selected provider's endpoint, auth, headers, and runtime
// API-key override. Other providers receive only their own ambient credentials.
type ModelCatalogQuery struct {
	Config                 *config.Config
	Providers              []string
	IncludeUnauthenticated bool
}

// ModelCatalogStatus records the outcome for one provider in an aggregate
// query. A provider error does not discard successful results from others.
type ModelCatalogStatus struct {
	Provider string
	Models   int
	Stale    bool
	Err      error
}

// ModelCatalogResult is the provider-neutral result consumed by model-listing
// surfaces. Partial results are valid; Status contains per-provider failures.
type ModelCatalogResult struct {
	Models []ModelMetadata
	Status []ModelCatalogStatus
	Stale  bool
}

type modelsDevCache struct {
	UpdatedAt int64            `json:"updated_at"`
	Created   map[string]int64 `json:"created"`
}

var (
	providerModelsMu       sync.RWMutex
	providerModelsOnce     sync.Once
	providerModelsCacheMap map[string]providerModelsCache
	modelsDevMu            sync.RWMutex
	modelsDevMeta          modelsDevCache
	providerCatalogFetcher providerCatalogFetcherFunc = fetchModels
	modelsDevFetcher                                  = fetchModelsDevCreated
	openAIFetcher          providerModelFetcher       = fetchOpenAIModels
	anthropicFetcher       providerModelFetcher       = fetchAnthropicModels
	openRouterFetcher      providerModelFetcher       = fetchOpenRouterModels
	geminiFetcher          providerModelFetcher       = fetchGeminiModels
	ollamaFetcher          providerModelFetcher       = fetchOllamaModels
	modelListHTTPClient                               = http.DefaultClient
)

type providerModelFetcher func(context.Context, *config.Config) ([]ModelMetadata, error)
type providerCatalogFetcherFunc func(context.Context, string, *config.Config) ([]ModelMetadata, error)

const (
	modelListRequestTimeout = 10 * time.Second
	localModelCacheTTL      = 10 * time.Second
	modelsDevTTL            = 24 * time.Hour
)

func ListModels(ctx context.Context, provider string) ([]ModelMetadata, error) {
	return ListModelsForConfig(ctx, &config.Config{Provider: provider})
}

func CachedModelsForConfig(cfg *config.Config) ([]ModelMetadata, bool, bool) {
	if cfg == nil {
		return nil, false, false
	}
	providerModelsOnce.Do(initProviderModelsCache)

	key := providerCacheKey(cfg)
	providerModelsMu.RLock()
	cached, ok := providerModelsCacheMap[key]
	providerModelsMu.RUnlock()
	if !ok {
		return nil, false, false
	}
	models := cloneModelMetadataSlice(cached.Models)
	sortModels(models)
	return models, cachedFreshForConfig(cached.UpdatedAt, cfg), true
}

func ListModelsForConfig(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
	result, err := QueryModelsForConfig(ctx, cfg)
	if err != nil {
		return nil, err
	}
	return result.Models, nil
}

// QueryModelsForConfig lists one provider and exposes whether a stale cache was
// used. ListModelsForConfig is the convenience form for callers that do not
// need freshness information.
func QueryModelsForConfig(ctx context.Context, cfg *config.Config) (ModelListResult, error) {
	if cfg == nil {
		return ModelListResult{}, fmt.Errorf("model provider config is required")
	}
	ctx, cancel, timeout := withModelListTimeout(ctx)
	defer cancel()

	providerModelsOnce.Do(initProviderModelsCache)

	key := providerCacheKey(cfg)
	providerModelsMu.RLock()
	cached, ok := providerModelsCacheMap[key]
	providerModelsMu.RUnlock()
	if ok && cachedFreshForConfig(cached.UpdatedAt, cfg) {
		return ModelListResult{
			Models:    cloneModelMetadataSlice(cached.Models),
			UpdatedAt: time.Unix(cached.UpdatedAt, 0),
		}, nil
	}

	fetched, err := providerCatalogFetcher(ctx, cfg.Provider, cfg)
	if err == nil {
		sortModels(fetched)
		updatedAt := time.Now()
		providerModelsMu.Lock()
		providerModelsCacheMap[key] = providerModelsCache{
			UpdatedAt: updatedAt.Unix(),
			Models:    cloneModelMetadataSlice(fetched),
		}
		saveProviderModelsCache()
		providerModelsMu.Unlock()
		return ModelListResult{
			Models:    cloneModelMetadataSlice(fetched),
			UpdatedAt: updatedAt,
		}, nil
	}

	if ok {
		return ModelListResult{
			Models:    cloneModelMetadataSlice(cached.Models),
			UpdatedAt: time.Unix(cached.UpdatedAt, 0),
			Stale:     true,
		}, nil
	}

	return ModelListResult{}, wrapModelListError(cfg, timeout, err)
}

// QueryAvailableModels queries all configured, listable native providers and
// returns the successful union. Providers without credentials are omitted by
// default, matching Pi's getAvailable behavior; set IncludeUnauthenticated to
// probe every requested provider and receive explicit failures in Status.
func QueryAvailableModels(ctx context.Context, query ModelCatalogQuery) (ModelCatalogResult, error) {
	base := query.Config
	if base == nil {
		base = &config.Config{}
	}

	providers := query.Providers
	if len(providers) == 0 {
		for _, def := range Native() {
			if def.SupportsModelListing {
				providers = append(providers, def.ID)
			}
		}
	}

	result := ModelCatalogResult{
		Models: make([]ModelMetadata, 0),
		Status: make([]ModelCatalogStatus, 0, len(providers)),
	}
	seenProviders := make(map[string]struct{}, len(providers))
	for _, rawProvider := range providers {
		provider := ResolveID(rawProvider)
		if provider == "" {
			continue
		}
		if _, seen := seenProviders[provider]; seen {
			continue
		}
		seenProviders[provider] = struct{}{}

		def, ok := Lookup(provider)
		if !ok || def.Runtime != RuntimeNative || !def.SupportsModelListing {
			result.Status = append(result.Status, ModelCatalogStatus{
				Provider: provider,
				Err:      fmt.Errorf("no model listing available for provider %s", provider),
			})
			continue
		}

		cfg := catalogConfigForProvider(base, provider)
		if !query.IncludeUnauthenticated && !catalogProviderConfigured(base, cfg, def) {
			continue
		}

		listed, err := QueryModelsForConfig(ctx, cfg)
		status := ModelCatalogStatus{
			Provider: provider,
			Models:   len(listed.Models),
			Stale:    listed.Stale,
			Err:      err,
		}
		result.Status = append(result.Status, status)
		if err != nil {
			continue
		}
		result.Models = append(result.Models, listed.Models...)
		result.Stale = result.Stale || listed.Stale
	}

	sortCatalogModels(result.Models)
	if len(result.Models) == 0 {
		for _, status := range result.Status {
			if status.Err != nil {
				return result, fmt.Errorf("no available models: %w", status.Err)
			}
		}
	}
	return result, nil
}

func catalogConfigForProvider(base *config.Config, provider string) *config.Config {
	clone := *base
	clone.Provider = provider
	if ResolveID(base.Provider) != provider {
		clone.Endpoint = ""
		clone.AuthEnvVar = ""
		clone.ExtraHeaders = nil
	}
	return &clone
}

func catalogProviderConfigured(base, cfg *config.Config, def Definition) bool {
	switch def.AuthKind {
	case AuthLocal:
		return ResolveID(base.Provider) == def.ID || strings.TrimSpace(os.Getenv("OLLAMA_HOST")) != ""
	case AuthOptional:
		return ResolveID(base.Provider) == def.ID && strings.TrimSpace(cfg.Endpoint) != ""
	}
	return ResolvedAuthToken(cfg, def) != ""
}

func sortCatalogModels(models []ModelMetadata) {
	slices.SortFunc(models, func(a, b ModelMetadata) int {
		if provider := strings.Compare(strings.ToLower(a.Provider), strings.ToLower(b.Provider)); provider != 0 {
			return provider
		}
		return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
	})
}

func withModelListTimeout(ctx context.Context) (context.Context, context.CancelFunc, time.Duration) {
	if _, ok := ctx.Deadline(); ok {
		return ctx, func() {}, 0
	}
	next, cancel := context.WithTimeout(ctx, modelListRequestTimeout)
	return next, cancel, modelListRequestTimeout
}

func wrapModelListError(cfg *config.Config, timeout time.Duration, err error) error {
	operation := "list models"
	if cfg != nil && strings.TrimSpace(cfg.Provider) != "" {
		operation = fmt.Sprintf("list models for %s", ResolveID(cfg.Provider))
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return ctxerr.Timeout(operation, timeout, err)
	}
	return ctxerr.WrapContext(operation, err)
}

func initProviderModelsCache() {
	providerModelsMu.Lock()
	defer providerModelsMu.Unlock()
	providerModelsCacheMap = make(map[string]providerModelsCache)
	loadProviderModelsCache()
}

func fetchModels(
	ctx context.Context,
	provider string,
	cfg *config.Config,
) ([]ModelMetadata, error) {
	provider = ResolveID(provider)
	switch provider {
	case "anthropic":
		return anthropicFetcher(ctx, cfg)
	case "openai":
		return openAIFetcher(ctx, cfg)
	case "openrouter":
		return openRouterFetcher(ctx, cfg)
	case "gemini":
		return geminiFetcher(ctx, cfg)
	case "ollama":
		if cfg != nil && strings.TrimSpace(cfg.Endpoint) != "" {
			endpoint := ResolvedEndpointContext(ctx, cfg)
			def, _ := Lookup(provider)
			return fetchOpenAICompatibleModels(
				ctx,
				provider,
				endpoint,
				ResolvedAuthToken(cfg, def),
				ResolvedHeaders(cfg),
			)
		}
		return ollamaFetcher(ctx, cfg)
	case OpenAICompatibleID:
		endpoint := ResolvedEndpointContext(ctx, cfg)
		if endpoint == "" {
			return nil, fmt.Errorf("OpenAI-compatible endpoint is not configured")
		}
		def, _ := Lookup(provider)
		return fetchOpenAICompatibleModels(
			ctx,
			provider,
			endpoint,
			ResolvedAuthToken(cfg, def),
			ResolvedHeaders(cfg),
		)
	default:
		def, ok := Lookup(provider)
		if !ok || def.Family != FamilyOpenAI {
			return nil, fmt.Errorf("no model listing available for provider %s", provider)
		}
		endpoint := ResolvedEndpointContext(ctx, cfg)
		if endpoint == "" {
			return nil, fmt.Errorf("provider %s has no configured endpoint", provider)
		}
		return fetchOpenAICompatibleModels(
			ctx,
			provider,
			endpoint,
			ResolvedAuthToken(cfg, def),
			ResolvedHeaders(cfg),
		)
	}
}

type openRouterModelsResponse struct {
	Data []openRouterModel `json:"data"`
}

type openAIModelsResponse struct {
	Data []openAIModel `json:"data"`
}

type openAIModel struct {
	ID      string `json:"id"`
	Created int64  `json:"created"`
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
	ID                  string                 `json:"id"`
	Name                string                 `json:"name"`
	ContextLength       int64                  `json:"context_length"`
	Pricing             openRouterPricing      `json:"pricing"`
	TopProvider         openRouterProvider     `json:"top_provider"`
	Architecture        openRouterArchitecture `json:"architecture"`
	Created             int64                  `json:"created"`
	SupportedParameters []string               `json:"supported_parameters"`
}

type openRouterPricing struct {
	Prompt     string `json:"prompt"`
	Completion string `json:"completion"`
}

type openRouterProvider struct {
	ContextLength       int64 `json:"context_length"`
	MaxCompletionTokens int64 `json:"max_completion_tokens"`
}

type openRouterArchitecture struct {
	InputModalities []string `json:"input_modalities"`
}

type geminiModelsResponse struct {
	Models        []geminiModel `json:"models"`
	NextPageToken string        `json:"nextPageToken"`
}

type geminiModel struct {
	Name                       string   `json:"name"`
	BaseModelID                string   `json:"baseModelId"`
	DisplayName                string   `json:"displayName"`
	InputTokenLimit            int      `json:"inputTokenLimit"`
	OutputTokenLimit           int      `json:"outputTokenLimit"`
	SupportedGenerationMethods []string `json:"supportedGenerationMethods"`
}

type ollamaTagsResponse struct {
	Models []ollamaModel `json:"models"`
}

type ollamaModel struct {
	Name string `json:"name"`
}

func fetchOpenAIModels(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
	def, _ := Lookup("openai")
	apiKey := ResolvedAuthToken(cfg, def)
	if apiKey == "" {
		return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
	}
	endpoint := catalogBaseURL(ctx, cfg, "https://api.openai.com/v1")
	headers := catalogHeaders(cfg)
	headers["Authorization"] = "Bearer " + apiKey

	var payload openAIModelsResponse
	if err := fetchJSON(ctx, http.MethodGet, strings.TrimRight(endpoint, "/")+"/models", headers, &payload); err != nil {
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
			Name:      id,
			Created:   model.Created,
			UpdatedAt: time.Now().Unix(),
		})
	}
	annotateCreated(ctx, models)
	return sortModels(models), nil
}

func fetchAnthropicModels(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
	def, _ := Lookup("anthropic")
	apiKey := ResolvedAuthToken(cfg, def)
	if apiKey == "" {
		return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
	}
	endpoint := catalogBaseURL(ctx, cfg, "https://api.anthropic.com/v1")
	headers := catalogHeaders(cfg)
	headers["X-Api-Key"] = apiKey
	headers["anthropic-version"] = "2023-06-01"

	var payload anthropicModelsResponse
	if err := fetchJSON(ctx, http.MethodGet, strings.TrimRight(endpoint, "/")+"/models?limit=1000", headers, &payload); err != nil {
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
			Name:         strings.TrimSpace(model.DisplayName),
			ContextLimit: model.MaxInputTokens,
			UpdatedAt:    time.Now().Unix(),
		})
	}
	annotateCreated(ctx, models)
	return sortModels(models), nil
}

func fetchOpenRouterModels(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
	def, _ := Lookup("openrouter")
	apiKey := ResolvedAuthToken(cfg, def)
	if apiKey == "" {
		return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
	}
	endpoint := catalogBaseURL(ctx, cfg, "https://openrouter.ai/api/v1")
	headers := catalogHeaders(cfg)
	headers["Authorization"] = "Bearer " + apiKey

	var payload openRouterModelsResponse
	if err := fetchJSON(ctx, http.MethodGet, strings.TrimRight(endpoint, "/")+"/models", headers, &payload); err != nil {
		return nil, fmt.Errorf("fetch openrouter models: %w", err)
	}

	models := make([]ModelMetadata, 0, len(payload.Data))
	for _, model := range payload.Data {
		contextLimit := int(model.ContextLength)
		if contextLimit == 0 {
			contextLimit = int(model.TopProvider.ContextLength)
		}
		inputPrice, inputKnown := parseMillionCost(model.Pricing.Prompt)
		outputPrice, outputKnown := parseMillionCost(model.Pricing.Completion)
		isReasoning := false
		for _, param := range model.SupportedParameters {
			if strings.ToLower(param) == "reasoning" {
				isReasoning = true
				break
			}
		}
		models = append(models, ModelMetadata{
			ID:               model.ID,
			Name:             model.Name,
			Provider:         "openrouter",
			ContextLimit:     contextLimit,
			MaxTokens:        int(model.TopProvider.MaxCompletionTokens),
			Input:            append([]string(nil), model.Architecture.InputModalities...),
			InputPrice:       inputPrice,
			OutputPrice:      outputPrice,
			InputPriceKnown:  inputKnown,
			OutputPriceKnown: outputKnown,
			Created:          model.Created,
			UpdatedAt:        time.Now().Unix(),
			Reasoning:        isReasoning,
		})
	}

	annotateCreated(ctx, models)
	return sortModels(models), nil
}

func fetchGeminiModels(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
	def, _ := Lookup("gemini")
	apiKey := ResolvedAuthToken(cfg, def)
	if apiKey == "" {
		return nil, fmt.Errorf("%s not set", MissingAuthDetail(cfg, def))
	}

	base := "https://generativelanguage.googleapis.com/v1beta/models"
	headers := catalogHeaders(cfg)
	models := make([]ModelMetadata, 0, 128)
	pageToken := ""
	for {
		endpoint := base + "?key=" + url.QueryEscape(apiKey) + "&pageSize=1000"
		if pageToken != "" {
			endpoint += "&pageToken=" + url.QueryEscape(pageToken)
		}

		var payload geminiModelsResponse
		if err := fetchJSON(ctx, http.MethodGet, endpoint, headers, &payload); err != nil {
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
				Name:         strings.TrimSpace(model.DisplayName),
				Provider:     "gemini",
				ContextLimit: model.InputTokenLimit,
				MaxTokens:    model.OutputTokenLimit,
				UpdatedAt:    time.Now().Unix(),
			})
		}

		if strings.TrimSpace(payload.NextPageToken) == "" {
			break
		}
		pageToken = payload.NextPageToken
	}

	annotateCreated(ctx, models)
	return sortModels(models), nil
}

func fetchOllamaModels(ctx context.Context, cfg *config.Config) ([]ModelMetadata, error) {
	base := normalizeOllamaBaseURL(strings.TrimSpace(os.Getenv("OLLAMA_HOST")))
	if cfg != nil && strings.TrimSpace(cfg.Endpoint) != "" {
		base = strings.TrimRight(cfg.Endpoint, "/")
		base = strings.TrimSuffix(base, "/v1")
	}
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
			Name:      id,
			Provider:  "ollama",
			Input:     []string{"text"},
			UpdatedAt: time.Now().Unix(),
		})
	}
	return sortModels(models), nil
}

func fetchOpenAICompatibleModels(
	ctx context.Context,
	provider, endpoint, token string,
	extraHeaders map[string]string,
) ([]ModelMetadata, error) {
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
			Name:      id,
			Provider:  provider,
			Input:     []string{"text"},
			UpdatedAt: time.Now().Unix(),
		})
	}
	return sortModels(models), nil
}

func catalogBaseURL(ctx context.Context, cfg *config.Config, fallback string) string {
	if cfg != nil {
		if def, ok := Lookup(cfg.Provider); ok && def.SupportsCustomEndpoint {
			if endpoint := ResolvedEndpointContext(ctx, cfg); endpoint != "" {
				return endpoint
			}
		}
	}
	return fallback
}

func catalogHeaders(cfg *config.Config) map[string]string {
	headers := cloneHeaders(ResolvedHeaders(cfg))
	if headers == nil {
		headers = make(map[string]string)
	}
	return headers
}

func parseMillionCost(raw string) (float64, bool) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return 0, false
	}
	value, err := strconv.ParseFloat(raw, 64)
	if err != nil {
		return 0, false
	}
	return value * 1_000_000, true
}

func cachedFreshForConfig(updatedAt int64, cfg *config.Config) bool {
	return cachedFreshWithin(updatedAt, modelCacheTTL(cfg))
}

func cachedFreshWithin(updatedAt int64, ttl time.Duration) bool {
	if updatedAt <= 0 {
		return false
	}
	return time.Since(time.Unix(updatedAt, 0)) < ttl
}

func modelCacheTTL(cfg *config.Config) time.Duration {
	if cfg == nil {
		return time.Duration(config.DefaultModelCacheTTLSeconds()) * time.Second
	}
	provider := ResolveID(cfg.Provider)
	switch provider {
	case OpenAICompatibleID, "ollama":
		return localModelCacheTTL
	}
	endpoint := strings.ToLower(
		strings.TrimSpace(ResolvedEndpointContext(context.Background(), cfg)),
	)
	if strings.Contains(endpoint, "://localhost") ||
		strings.Contains(endpoint, "://127.") ||
		strings.Contains(endpoint, "://[::1]") {
		return localModelCacheTTL
	}
	return time.Duration(config.DefaultModelCacheTTLSeconds()) * time.Second
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

func fetchJSON(
	ctx context.Context,
	method, endpoint string,
	headers map[string]string,
	into any,
) error {
	req, err := http.NewRequestWithContext(ctx, method, endpoint, nil)
	if err != nil {
		return fmt.Errorf("build request: %w", err)
	}
	req.Header.Set("User-Agent", "ion/0.0.0")
	for key, value := range headers {
		req.Header.Set(key, value)
	}

	resp, err := modelListHTTPClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("read response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf(
			"%s returned %d: %s",
			endpoint,
			resp.StatusCode,
			strings.TrimSpace(string(body)),
		)
	}
	if err := json.Unmarshal(body, into); err != nil {
		return fmt.Errorf("decode response: %w", err)
	}
	return nil
}

func sortModels(models []ModelMetadata) []ModelMetadata {
	slices.SortFunc(models, func(a, b ModelMetadata) int {
		if orgA, orgB := modelOrg(a.ID), modelOrg(b.ID); orgA != orgB {
			return strings.Compare(orgA, orgB)
		}
		if a.Created != b.Created {
			if a.Created > b.Created {
				return -1
			}
			return 1
		}
		return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
	})
	return models
}

func annotateCreated(ctx context.Context, models []ModelMetadata) {
	index := modelsDevCreatedIndex(ctx)
	for i := range models {
		if models[i].Created <= 0 {
			models[i].Created = index[strings.ToLower(models[i].ID)]
		}
		if models[i].Created <= 0 {
			models[i].Created = inferCreatedFromModelID(models[i].ID)
		}
	}
}

func modelOrg(id string) string {
	left, _, ok := strings.Cut(strings.ToLower(strings.TrimSpace(id)), "/")
	if !ok {
		return ""
	}
	return left
}

func modelsDevCreatedIndex(ctx context.Context) map[string]int64 {
	modelsDevMu.RLock()
	if len(modelsDevMeta.Created) > 0 &&
		time.Since(time.Unix(modelsDevMeta.UpdatedAt, 0)) < modelsDevTTL {
		index := mapsCloneInt64(modelsDevMeta.Created)
		modelsDevMu.RUnlock()
		return index
	}
	modelsDevMu.RUnlock()

	created, err := modelsDevFetcher(ctx)
	if err != nil {
		modelsDevMu.RLock()
		index := mapsCloneInt64(modelsDevMeta.Created)
		modelsDevMu.RUnlock()
		return index
	}

	modelsDevMu.Lock()
	modelsDevMeta = modelsDevCache{
		UpdatedAt: time.Now().Unix(),
		Created:   mapsCloneInt64(created),
	}
	modelsDevMu.Unlock()
	return created
}

type modelsDevProvider struct {
	Models map[string]modelsDevEntry `json:"models"`
}

type modelsDevEntry struct {
	ReleaseDate string `json:"release_date"`
}

func fetchModelsDevCreated(ctx context.Context) (map[string]int64, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://models.dev/api.json", nil)
	if err != nil {
		return nil, fmt.Errorf("build models.dev request: %w", err)
	}
	req.Header.Set("User-Agent", "ion/0.0.0")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("read models.dev response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf(
			"models.dev returned %d: %s",
			resp.StatusCode,
			strings.TrimSpace(string(body)),
		)
	}
	var payload map[string]modelsDevProvider
	if err := json.Unmarshal(body, &payload); err != nil {
		return nil, fmt.Errorf("decode models.dev response: %w", err)
	}
	out := make(map[string]int64, 1024)
	for _, provider := range payload {
		for modelID, entry := range provider.Models {
			key := strings.ToLower(strings.TrimSpace(modelID))
			if key == "" {
				continue
			}
			out[key] = parseReleaseDate(entry.ReleaseDate)
		}
	}
	return out, nil
}

func parseReleaseDate(date string) int64 {
	value := strings.TrimSpace(date)
	if value == "" {
		return 0
	}
	t, err := time.Parse("2006-01-02", value)
	if err != nil {
		return 0
	}
	return t.Unix()
}

func inferCreatedFromModelID(id string) int64 {
	value := strings.ToLower(strings.TrimSpace(id))
	if value == "" {
		return 0
	}
	if ts := scanModelDateSubstrings(value); ts > 0 {
		return ts
	}
	for _, token := range splitModelTokens(value) {
		if ts := parseModelTokenDate(token); ts > 0 {
			return ts
		}
	}
	return 0
}

func scanModelDateSubstrings(value string) int64 {
	for i := 0; i+10 <= len(value); i++ {
		if ts := parseModelTokenDate(value[i : i+10]); ts > 0 {
			return ts
		}
	}
	for i := 0; i+8 <= len(value); i++ {
		if ts := parseModelTokenDate(value[i : i+8]); ts > 0 {
			return ts
		}
	}
	for i := 0; i+6 <= len(value); i++ {
		if ts := parseModelTokenDate(value[i : i+6]); ts > 0 {
			return ts
		}
	}
	for i := 0; i+4 <= len(value); i++ {
		if ts := parseModelTokenDate(value[i : i+4]); ts > 0 {
			return ts
		}
	}
	return 0
}

func splitModelTokens(value string) []string {
	return strings.FieldsFunc(value, func(r rune) bool {
		switch r {
		case '/', '-', '_', '.', ':':
			return true
		default:
			return false
		}
	})
}

func parseModelTokenDate(token string) int64 {
	if len(token) == 10 && token[4] == '-' && token[7] == '-' {
		return parseReleaseDate(token)
	}
	if len(token) == 8 && allDigits(token) {
		t, err := time.Parse("20060102", token)
		if err == nil {
			return t.Unix()
		}
	}
	if len(token) == 6 && allDigits(token) {
		t, err := time.Parse("20060102", "20"+token)
		if err == nil {
			return t.Unix()
		}
	}
	if len(token) == 4 && allDigits(token) {
		t, err := time.Parse("200601", "20"+token[:2]+token[2:])
		if err == nil {
			return t.Unix()
		}
	}
	return 0
}

func allDigits(value string) bool {
	for _, r := range value {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}

func mapsCloneInt64(src map[string]int64) map[string]int64 {
	if len(src) == 0 {
		return map[string]int64{}
	}
	dst := make(map[string]int64, len(src))
	for k, v := range src {
		dst[k] = v
	}
	return dst
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
	provider := ResolveID(cfg.Provider)
	endpoint := ResolvedEndpointContext(context.Background(), cfg)
	authEnv := ResolvedAuthEnvVar(cfg)
	def, _ := Lookup(provider)
	auth := ResolvedAuthToken(cfg, def)
	headers := ResolvedHeaders(cfg)
	headerKeys := make([]string, 0, len(headers))
	for key := range headers {
		headerKeys = append(headerKeys, key)
	}
	slices.Sort(headerKeys)
	fingerprintInput := strings.Builder{}
	fingerprintInput.WriteString(auth)
	for _, key := range headerKeys {
		fingerprintInput.WriteString("\x00")
		fingerprintInput.WriteString(key)
		fingerprintInput.WriteString("=")
		fingerprintInput.WriteString(headers[key])
	}
	digest := sha256.Sum256([]byte(fingerprintInput.String()))
	return strings.Join([]string{
		provider,
		strings.ToLower(strings.TrimSpace(endpoint)),
		strings.TrimSpace(authEnv),
		fmt.Sprintf("%x", digest[:8]),
	}, "|")
}

func cloneModelMetadataSlice(models []ModelMetadata) []ModelMetadata {
	if len(models) == 0 {
		return nil
	}
	out := make([]ModelMetadata, len(models))
	copy(out, models)
	for i := range out {
		out[i].Input = slices.Clone(models[i].Input)
	}
	return out
}
