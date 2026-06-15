package harness

import (
	"fmt"
	"sync"

	"github.com/nijaru/ion/llm"
)

// ModelRouter dynamically selects models based on request properties.
type ModelRouter struct {
	providers map[string]llm.Provider
	models    map[string]llm.Model
	rules     []RoutingRule
	default_  string // default model ID
	mu        sync.RWMutex
}

// RoutingRule matches requests to specific models.
type RoutingRule struct {
	// Name is a human-readable identifier for the rule.
	Name string
	// Condition returns true if this rule applies to the request.
	Condition func(req *llm.Request) bool
	// ModelID is the model to use when this rule matches.
	ModelID string
}

// NewModelRouter creates a new model router.
func NewModelRouter() *ModelRouter {
	return &ModelRouter{
		providers: make(map[string]llm.Provider),
		models:    make(map[string]llm.Model),
	}
}

// RegisterProvider registers a provider with the router.
func (r *ModelRouter) RegisterProvider(name string, provider llm.Provider) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.providers[name] = provider
}

// RegisterModel registers a model with the router.
func (r *ModelRouter) RegisterModel(model llm.Model) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.models[model.ID] = model
}

// SetDefault sets the default model ID.
func (r *ModelRouter) SetDefault(modelID string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.default_ = modelID
}

// AddRule adds a routing rule. Rules are evaluated in order; first match wins.
func (r *ModelRouter) AddRule(rule RoutingRule) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.rules = append(r.rules, rule)
}

// Route selects a model and provider for the given request.
// Returns the provider, model, and the actual model ID used.
func (r *ModelRouter) Route(req *llm.Request) (llm.Provider, llm.Model, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()

	// Evaluate rules in order
	for _, rule := range r.rules {
		if rule.Condition(req) {
			return r.resolveModel(rule.ModelID)
		}
	}

	// Fall back to request model or default
	modelID := req.Model
	if modelID == "" {
		modelID = r.default_
	}
	if modelID == "" {
		return nil, llm.Model{}, fmt.Errorf("no model specified and no default set")
	}

	return r.resolveModel(modelID)
}

// resolveModel finds the provider and model for the given model ID.
func (r *ModelRouter) resolveModel(modelID string) (llm.Provider, llm.Model, error) {
	model, ok := r.models[modelID]
	if !ok {
		return nil, llm.Model{}, fmt.Errorf("model %q not registered", modelID)
	}

	providerName := model.Provider
	if providerName == "" {
		return nil, llm.Model{}, fmt.Errorf("model %q has no provider", modelID)
	}

	provider, ok := r.providers[providerName]
	if !ok {
		return nil, llm.Model{}, fmt.Errorf("provider %q not registered", providerName)
	}

	return provider, model, nil
}

// Models returns all registered models.
func (r *ModelRouter) Models() []llm.Model {
	r.mu.RLock()
	defer r.mu.RUnlock()
	result := make([]llm.Model, 0, len(r.models))
	for _, m := range r.models {
		result = append(result, m)
	}
	return result
}

// Providers returns all registered provider names.
func (r *ModelRouter) Providers() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	result := make([]string, 0, len(r.providers))
	for name := range r.providers {
		result = append(result, name)
	}
	return result
}
