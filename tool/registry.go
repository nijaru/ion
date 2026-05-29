package tool

import (
	"context"
	"fmt"
	"slices"
	"sync"

	"github.com/nijaru/ion/llm"
)

// Registry manages the available tools.
type Registry struct {
	mu    sync.RWMutex
	tools map[string]Tool
}

// NewRegistry creates a new tool registry.
func NewRegistry() *Registry {
	return &Registry{
		tools: make(map[string]Tool),
	}
}

// Register adds a tool to the registry.
func (r *Registry) Register(t Tool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.tools[t.Spec().Name] = t
}

// Get returns a tool by its name.
func (r *Registry) Get(name string) (Tool, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	t, ok := r.tools[name]
	return t, ok
}

// Specs returns all tool specifications.
func (r *Registry) Specs() []*llm.Spec {
	entries := r.Entries()
	res := make([]*llm.Spec, 0, len(entries))
	for _, entry := range entries {
		spec := entry.Spec
		res = append(res, &spec)
	}
	return res
}

// Entries returns the registered tools with their metadata.
func (r *Registry) Entries() []ToolEntry {
	r.mu.RLock()
	defer r.mu.RUnlock()
	names := make([]string, 0, len(r.tools))
	for name := range r.tools {
		names = append(names, name)
	}
	slices.Sort(names)

	res := make([]ToolEntry, 0, len(names))
	for _, name := range names {
		t := r.tools[name]
		res = append(res, ToolEntry{
			Name:     name,
			Tool:     t,
			Spec:     t.Spec(),
			Metadata: MetadataFor(t),
		})
	}
	return res
}

// Metadata returns metadata for a tool by name.
func (r *Registry) Metadata(name string) (Metadata, bool) {
	t, ok := r.Get(name)
	if !ok {
		return Metadata{}, false
	}
	return MetadataFor(t), true
}

// Names returns the names of all registered tools.
func (r *Registry) Names() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	names := make([]string, 0, len(r.tools))
	for name := range r.tools {
		names = append(names, name)
	}
	slices.Sort(names)
	return names
}

// Subset returns a new registry containing only the named tools.
// It fails closed if any requested tool is missing.
func (r *Registry) Subset(names ...string) (*Registry, error) {
	subset := NewRegistry()
	for _, name := range names {
		t, ok := r.Get(name)
		if !ok {
			return nil, fmt.Errorf("tool not found: %s", name)
		}
		subset.Register(t)
	}
	return subset, nil
}

// Execute looks up and runs a tool.
func (r *Registry) Execute(ctx context.Context, name, args string) (string, error) {
	t, ok := r.Get(name)
	if !ok {
		return "", fmt.Errorf("tool not found: %s", name)
	}
	return t.Execute(ctx, args)
}
