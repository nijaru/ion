package llm

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/ion/config"
)

type ModelMetadata struct {
	ID               string   `json:"id"`
	Provider         string   `json:"provider"`
	Name             string   `json:"name,omitempty"`
	ContextLimit     int      `json:"context_limit"`
	MaxTokens        int      `json:"max_tokens"`
	Input            []string `json:"input,omitempty"`
	InputPrice       float64  `json:"input_price"`  // per 1M tokens
	OutputPrice      float64  `json:"output_price"` // per 1M tokens
	InputPriceKnown  bool     `json:"input_price_known"`
	OutputPriceKnown bool     `json:"output_price_known"`
	Created          int64    `json:"created"`
	UpdatedAt        int64    `json:"updated_at"`
	Reasoning        bool     `json:"reasoning"`
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

	if meta, ok := cachedMetadata(provider, model); ok {
		return meta, true
	}

	fetched, err := metadataFetcher(ctx, provider, model)
	if err == nil {
		registryMu.Lock()
		registryCache[metadataKey(provider, model)] = fetched
		saveCache()
		registryMu.Unlock()
		return fetched, true
	}

	return ModelMetadata{}, false
}

func GetCachedMetadata(provider, model string) (ModelMetadata, bool) {
	registryOnce.Do(initRegistry)
	return cachedMetadata(provider, model)
}

func CachedContextLimit(provider, model string) (int, bool) {
	meta, ok := GetCachedMetadata(provider, model)
	if !ok || meta.ContextLimit <= 0 {
		return 0, false
	}
	return meta.ContextLimit, true
}

func CachedContextLimitForConfig(cfg *config.Config) (int, bool) {
	if cfg == nil || strings.TrimSpace(cfg.Model) == "" {
		return 0, false
	}
	if limit, ok := CachedContextLimit(cfg.Provider, cfg.Model); ok {
		return limit, true
	}
	if IsOpenAICompatible(cfg.Provider) && strings.TrimSpace(cfg.Endpoint) == "" {
		return 0, false
	}
	models, _, ok := CachedModelsForConfig(cfg)
	if !ok {
		return 0, false
	}
	for _, meta := range models {
		if strings.EqualFold(meta.ID, cfg.Model) && meta.ContextLimit > 0 {
			return meta.ContextLimit, true
		}
	}
	return 0, false
}

func cachedMetadata(provider, model string) (ModelMetadata, bool) {
	registryMu.RLock()
	meta, ok := registryCache[metadataKey(provider, model)]
	registryMu.RUnlock()

	if ok && time.Now().Unix()-meta.UpdatedAt < 86400 {
		return meta, true
	}
	return ModelMetadata{}, false
}

func metadataKey(provider, model string) string {
	return fmt.Sprintf("%s/%s", strings.ToLower(provider), strings.ToLower(model))
}

func fetchMetadata(ctx context.Context, provider, model string) (ModelMetadata, error) {
	if def, ok := Lookup(provider); ok && def.Runtime == RuntimeNative {
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
	return ModelMetadata{}, fmt.Errorf(
		"no live metadata catalog configured for provider %s",
		provider,
	)
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
	_ = os.MkdirAll(filepath.Dir(path), 0o755)
	_ = os.WriteFile(path, data, 0o644)
}

// ModelPreset defines standard capability profiles.
type ModelPreset string

const (
	PresetChat            ModelPreset = "chat"
	PresetReasoning       ModelPreset = "reasoning"
	PresetOpenAIReasoning ModelPreset = "openai-reasoning"
)

// ModelDef represents a model capability mapping definition.
type ModelDef struct {
	Pattern      string        `json:"pattern"                toml:"pattern"` // glob pattern (e.g. "deepseek-*") or exact name
	Preset       ModelPreset   `json:"preset,omitempty"       toml:"preset,omitempty"`
	Capabilities *Capabilities `json:"capabilities,omitempty" toml:"capabilities,omitempty"`
}

// Registry manages thread-safe resolution of model capabilities.
type Registry struct {
	mu   sync.RWMutex
	defs []ModelDef
}

// NewRegistry creates a new Model Capability Registry.
func NewRegistry() *Registry {
	return &Registry{
		defs: make([]ModelDef, 0),
	}
}

// Clear clears all registered model definitions.
func (r *Registry) Clear() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.defs = r.defs[:0]
}

// Register registers a new model capability definition.
func (r *Registry) Register(def ModelDef) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.defs = append(r.defs, def)
}

// Resolve resolves capabilities for a given model ID.
func (r *Registry) Resolve(modelID string) Capabilities {
	r.mu.RLock()
	defer r.mu.RUnlock()

	modelLower := strings.ToLower(strings.TrimSpace(modelID))
	for _, def := range r.defs {
		patternLower := strings.ToLower(strings.TrimSpace(def.Pattern))

		// Try glob matching first
		if matched, err := filepath.Match(patternLower, modelLower); err == nil && matched {
			return r.capabilitiesFromDef(def, modelLower)
		}

		// Fallback to substring matching by stripping wildcards
		cleanPattern := strings.Trim(patternLower, "*")
		if cleanPattern != "" && strings.Contains(modelLower, cleanPattern) {
			return r.capabilitiesFromDef(def, modelLower)
		}
	}

	return DefaultCapabilities()
}

func (r *Registry) capabilitiesFromDef(def ModelDef, modelID string) Capabilities {
	if def.Capabilities != nil {
		return *def.Capabilities
	}

	switch def.Preset {
	case PresetChat:
		return DefaultCapabilities()
	case PresetReasoning:
		return Capabilities{
			Streaming:   true,
			Tools:       true,
			Temperature: false,
			SystemRole:  RoleSystem,
			Reasoning: ReasoningCapabilities{
				Kind:       ReasoningKindEffort,
				Efforts:    []string{"minimal", "low", "medium", "high"},
				CanDisable: true,
			},
		}
	case PresetOpenAIReasoning:
		role := RoleSystem
		if strings.Contains(modelID, "o1") {
			role = RoleUser
		} else if strings.Contains(modelID, "o3") || strings.Contains(modelID, "o4") {
			role = RoleDeveloper
		}
		return Capabilities{
			Streaming:   true,
			Tools:       true,
			Temperature: false,
			SystemRole:  role,
			Reasoning: ReasoningCapabilities{
				Kind:       ReasoningKindEffort,
				Efforts:    []string{"minimal", "low", "medium", "high"},
				CanDisable: true,
			},
		}
	default:
		return DefaultCapabilities()
	}
}

// DefaultRegistry is the framework-wide capability registry.
var DefaultRegistry = NewRegistry()

func init() {
	// Start with an empty registry. All capability presets and matching definitions
	// should be registered dynamically by the host application or dynamically discovered.
}

// RegisterModel registers a model capability definition globally.
func RegisterModel(def ModelDef) {
	DefaultRegistry.Register(def)
}

// ResolveCapabilities resolves model capabilities globally.
func ResolveCapabilities(model string) Capabilities {
	return DefaultRegistry.Resolve(model)
}

// ClearRegistry clears all definitions from the global registry.
func ClearRegistry() {
	DefaultRegistry.Clear()
}
