package tool

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

// staticTool is a minimal Tool implementation for registry tests.
type staticTool struct {
	name   string
	result string
}

func (s *staticTool) Spec() llm.Spec {
	return llm.Spec{
		Name:        s.name,
		Description: "A static test tool.",
		Parameters:  map[string]any{"type": "object", "properties": map[string]any{}},
	}
}

func (s *staticTool) Execute(_ context.Context, _ string) (string, error) {
	return s.result, nil
}

type metadataTool struct {
	staticTool
	metadata Metadata
}

func (m *metadataTool) Metadata() Metadata { return m.metadata }

func TestRegistry_NewRegistry(t *testing.T) {
	reg := NewRegistry()
	if reg == nil {
		t.Fatal("NewRegistry returned nil")
	}
	if len(reg.Specs()) != 0 {
		t.Errorf("expected empty registry, got %d tools", len(reg.Specs()))
	}
}

func TestRegistry_RegisterAndGet(t *testing.T) {
	reg := NewRegistry()
	tool := &staticTool{name: "my_tool", result: "ok"}
	reg.Register(tool)

	got, ok := reg.Get("my_tool")
	if !ok {
		t.Fatal("expected tool to be found")
	}
	if got.Spec().Name != "my_tool" {
		t.Errorf("expected name 'my_tool', got %q", got.Spec().Name)
	}
}

func TestRegistry_GetNotFound(t *testing.T) {
	reg := NewRegistry()
	_, ok := reg.Get("nonexistent")
	if ok {
		t.Error("expected false for missing tool, got true")
	}
}

func TestRegistry_Specs(t *testing.T) {
	reg := NewRegistry()
	reg.Register(&staticTool{name: "alpha", result: "a"})
	reg.Register(&staticTool{name: "beta", result: "b"})

	specs := reg.Specs()
	if len(specs) != 2 {
		t.Fatalf("expected 2 specs, got %d", len(specs))
	}
	names := make(map[string]bool, 2)
	for _, s := range specs {
		names[s.Name] = true
	}
	if !names["alpha"] || !names["beta"] {
		t.Errorf("unexpected specs: %v", specs)
	}
}

func TestRegistry_SpecsAndNames_AreSorted(t *testing.T) {
	reg := NewRegistry()
	reg.Register(&staticTool{name: "zeta", result: "z"})
	reg.Register(&staticTool{name: "alpha", result: "a"})
	reg.Register(&staticTool{name: "beta", result: "b"})

	names := reg.Names()
	if got, want := strings.Join(names, ","), "alpha,beta,zeta"; got != want {
		t.Fatalf("registry names = %q, want %q", got, want)
	}

	specs := reg.Specs()
	if got := []string{specs[0].Name, specs[1].Name, specs[2].Name}; strings.Join(
		got,
		",",
	) != "alpha,beta,zeta" {
		t.Fatalf("registry specs order = %v, want [alpha beta zeta]", got)
	}
}

func TestRegistry_Execute_Found(t *testing.T) {
	reg := NewRegistry()
	reg.Register(&staticTool{name: "greeter", result: "hello"})

	result, err := reg.Execute(context.Background(), "greeter", "{}")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if result != "hello" {
		t.Errorf("expected 'hello', got %q", result)
	}
}

func TestRegistry_Execute_NotFound(t *testing.T) {
	reg := NewRegistry()
	_, err := reg.Execute(context.Background(), "missing", "{}")
	if err == nil {
		t.Fatal("expected error for missing tool, got nil")
	}
	if !strings.Contains(err.Error(), "tool not found: missing") {
		t.Errorf("unexpected error message: %v", err)
	}
}

func TestRegistry_EntriesIncludeMetadata(t *testing.T) {
	reg := NewRegistry()
	reg.Register(&metadataTool{
		staticTool: staticTool{name: "search", result: "ok"},
		metadata: Metadata{
			Category:    "workspace",
			ReadOnly:    true,
			Concurrency: Parallel,
			Deferred:    true,
			Examples: []Example{{
				Description: "Search Go files",
				Arguments:   `{"pattern":"TODO"}`,
			}},
		},
	})

	entries := reg.Entries()
	if len(entries) != 1 {
		t.Fatalf("expected 1 entry, got %d", len(entries))
	}
	if entries[0].Metadata.Category != "workspace" {
		t.Fatalf("metadata category = %q, want workspace", entries[0].Metadata.Category)
	}
	if !entries[0].Metadata.ReadOnly || !entries[0].Metadata.Deferred {
		t.Fatalf("unexpected metadata flags: %+v", entries[0].Metadata)
	}
}

func TestRegistry_Metadata(t *testing.T) {
	reg := NewRegistry()
	reg.Register(&metadataTool{
		staticTool: staticTool{name: "grep", result: "ok"},
		metadata: Metadata{
			Concurrency: Parallel,
		},
	})

	got, ok := reg.Metadata("grep")
	if !ok {
		t.Fatal("expected metadata to exist")
	}
	if got.Concurrency != Parallel {
		t.Fatalf("concurrency = %q, want %q", got.Concurrency, Parallel)
	}
}

func TestRegistry_Subset(t *testing.T) {
	reg := NewRegistry()
	reg.Register(&staticTool{name: "alpha", result: "a"})
	reg.Register(&staticTool{name: "beta", result: "b"})

	subset, err := reg.Subset("beta")
	if err != nil {
		t.Fatalf("subset: %v", err)
	}

	if got := strings.Join(subset.Names(), ","); got != "beta" {
		t.Fatalf("subset names = %q, want beta", got)
	}
}

func TestRegistry_SubsetMissingToolFailsClosed(t *testing.T) {
	reg := NewRegistry()
	reg.Register(&staticTool{name: "alpha", result: "a"})

	if _, err := reg.Subset("missing"); err == nil {
		t.Fatal("expected missing subset tool to fail")
	}
}
