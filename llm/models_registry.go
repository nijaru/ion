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

func (c *ModelCatalog) GetMetadata(ctx context.Context, provider, model string) (ModelMetadata, bool) {
	if meta, ok := c.cachedMetadata(provider, model); ok {
		return meta, true
	}

	fetched, err := c.metadataFetcher(ctx, provider, model)
	if err == nil {
		c.metadataMu.Lock()
		c.metadataCache[metadataKey(provider, model)] = fetched
		c.saveMetadataCache()
		c.metadataMu.Unlock()
		return fetched, true
	}

	return ModelMetadata{}, false
}

func (c *ModelCatalog) GetCachedMetadata(provider, model string) (ModelMetadata, bool) {
	return c.cachedMetadata(provider, model)
}

func (c *ModelCatalog) CachedContextLimit(provider, model string) (int, bool) {
	meta, ok := c.GetCachedMetadata(provider, model)
	if !ok || meta.ContextLimit <= 0 {
		return 0, false
	}
	return meta.ContextLimit, true
}

func (c *ModelCatalog) CachedContextLimitForConfig(cfg *config.Config) (int, bool) {
	if cfg == nil || strings.TrimSpace(cfg.Model) == "" {
		return 0, false
	}
	if limit, ok := c.CachedContextLimit(cfg.Provider, cfg.Model); ok {
		return limit, true
	}
	if IsOpenAICompatible(cfg.Provider) && strings.TrimSpace(cfg.Endpoint) == "" {
		return 0, false
	}
	models, _, ok := c.CachedModelsForConfig(cfg)
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

func (c *ModelCatalog) cachedMetadata(provider, model string) (ModelMetadata, bool) {
	c.metadataMu.RLock()
	meta, ok := c.metadataCache[metadataKey(provider, model)]
	c.metadataMu.RUnlock()

	if ok && time.Now().Unix()-meta.UpdatedAt < 86400 {
		return meta, true
	}
	return ModelMetadata{}, false
}

func metadataKey(provider, model string) string {
	return fmt.Sprintf("%s/%s", strings.ToLower(provider), strings.ToLower(model))
}

func (c *ModelCatalog) fetchMetadata(ctx context.Context, provider, model string) (ModelMetadata, error) {
	if _, ok := Lookup(provider); ok {
		models, err := c.ListModelsForConfig(ctx, &config.Config{Provider: provider})
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

func (c *ModelCatalog) metadataCachePath() string {
	return filepath.Join(c.dataDir, "metadata_cache.json")
}

func (c *ModelCatalog) loadMetadataCache() {
	data, err := os.ReadFile(c.metadataCachePath())
	if err != nil {
		return
	}
	_ = json.Unmarshal(data, &c.metadataCache)
}

func (c *ModelCatalog) saveMetadataCache() {
	data, err := json.MarshalIndent(c.metadataCache, "", "  ")
	if err != nil {
		return
	}
	path := c.metadataCachePath()
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
	Pattern       string        `json:"pattern"                toml:"pattern"` // glob pattern (e.g. "deepseek-*") or exact name
	Preset        ModelPreset   `json:"preset,omitempty"       toml:"preset,omitempty"`
	Temperature   *bool         `json:"temperature,omitempty"   toml:"temperature,omitempty"`
	SystemRole    Role          `json:"system_role,omitempty"   toml:"system_role,omitempty"`
	ReasoningKind ReasoningKind `json:"reasoning_kind,omitempty" toml:"reasoning_kind,omitempty"`
	Capabilities  *Capabilities `json:"capabilities,omitempty" toml:"capabilities,omitempty"`
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

	caps := DefaultCapabilities()
	switch def.Preset {
	case PresetChat:
		// DefaultCapabilities is already the chat profile.
	case PresetReasoning:
		caps.Temperature = false
		caps.Reasoning = ReasoningCapabilities{
			Kind:       ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high"},
			CanDisable: true,
		}
	case PresetOpenAIReasoning:
		role := RoleSystem
		if strings.Contains(modelID, "o1") {
			role = RoleUser
		} else if strings.Contains(modelID, "o3") || strings.Contains(modelID, "o4") {
			role = RoleDeveloper
		}
		caps.Temperature = false
		caps.SystemRole = role
		caps.Reasoning = ReasoningCapabilities{
			Kind:       ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high"},
			CanDisable: true,
		}
	}

	if def.Temperature != nil {
		caps.Temperature = *def.Temperature
	}
	if def.SystemRole != "" {
		caps.SystemRole = def.SystemRole
	}
	if def.ReasoningKind != ReasoningKindNone {
		caps.Reasoning = reasoningCapabilitiesForKind(def.ReasoningKind)
	}
	return caps
}

func reasoningCapabilitiesForKind(kind ReasoningKind) ReasoningCapabilities {
	switch kind {
	case ReasoningKindEffort:
		return ReasoningCapabilities{
			Kind:       ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high", "xhigh"},
			CanDisable: true,
		}
	case ReasoningKindBudget:
		return ReasoningCapabilities{
			Kind:                ReasoningKindBudget,
			CanDisable:          true,
			BudgetMinTokens:     defaultThinkingBudgetMinTokens,
			BudgetDefaultTokens: defaultThinkingBudgetTokens,
			BudgetMaxTokens:     defaultThinkingBudgetMaxTokens,
		}
	case ReasoningKindBoolean:
		return ReasoningCapabilities{
			Kind:       ReasoningKindBoolean,
			CanDisable: true,
		}
	default:
		return ReasoningCapabilities{}
	}
}
