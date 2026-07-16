package main

import (
	"context"
	"slices"
	"testing"

	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

func TestThinkingLevelForRuntime(t *testing.T) {
	tests := []struct {
		name  string
		value string
		want  session.ThinkingLevel
	}{
		{name: "auto uses provider default", value: "auto", want: ""},
		{name: "empty uses provider default", value: "", want: ""},
		{name: "minimal", value: "minimal", want: session.ThinkingMinimal},
		{name: "xhigh", value: "xhigh", want: session.ThinkingXHigh},
		{name: "provider-specific max", value: "max", want: "max"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := thinkingLevelForRuntime(tt.value); got != tt.want {
				t.Fatalf("thinkingLevelForRuntime(%q) = %q, want %q", tt.value, got, tt.want)
			}
		})
	}
}

func TestActiveToolNamesForMode(t *testing.T) {
	registry := tool.NewRegistry()
	for _, name := range []string{"bash", "edit", "read", "search_tools", "write", "find", "grep", "ls", "read_skill"} {
		registry.Register(tool.Func(name, name, map[string]any{"type": "object"}, func(context.Context, string) (string, error) {
			return "", nil
		}))
	}
	tests := []struct {
		mode string
		want []string
	}{
		{mode: "coding", want: []string{"bash", "edit", "read", "search_tools", "write"}},
		{mode: "read", want: []string{"find", "grep", "ls", "read", "search_tools"}},
		{mode: "all", want: []string{"bash", "edit", "find", "grep", "ls", "read", "read_skill", "search_tools", "write"}},
		{mode: "unknown", want: []string{"bash", "edit", "read", "search_tools", "write"}},
	}
	for _, tt := range tests {
		t.Run(tt.mode, func(t *testing.T) {
			if got := activeToolNamesForMode(registry, tt.mode); !slices.Equal(got, tt.want) {
				t.Fatalf("activeToolNamesForMode(%q) = %#v, want %#v", tt.mode, got, tt.want)
			}
		})
	}
}

func TestDefaultActiveToolNamesKeepsDiscoveryMetaTool(t *testing.T) {
	registry := tool.NewRegistry()
	for _, name := range []string{"bash", "edit", "read", "search_tools", "write", "find", "grep"} {
		registry.Register(tool.Func(name, name, map[string]any{"type": "object"}, func(context.Context, string) (string, error) {
			return "", nil
		}))
	}
	if got := defaultActiveToolNames(registry); !slices.Equal(got, []string{"bash", "edit", "read", "search_tools", "write"}) {
		t.Fatalf("default active tools = %#v", got)
	}
}

func TestActiveToolNamesIncludesConfiguredMCPTools(t *testing.T) {
	registry := tool.NewRegistry()
	for _, name := range []string{"bash", "edit", "read", "search_tools", "write"} {
		registry.Register(tool.Func(name, name, map[string]any{"type": "object"}, func(context.Context, string) (string, error) {
			return "", nil
		}))
	}
	registry.Register(tool.FuncWithMetadata(
		"mcp_workspace_echo",
		"remote echo",
		map[string]any{"type": "object"},
		tool.Metadata{Category: "mcp"},
		func(context.Context, string) (string, error) { return "", nil },
	))
	got := activeToolNamesForMode(registry, "coding")
	if !slices.Equal(got, []string{"bash", "edit", "read", "search_tools", "write", "mcp_workspace_echo"}) {
		t.Fatalf("active MCP tools = %#v", got)
	}
}

func TestActiveToolNamesRespectOptInMemorySurface(t *testing.T) {
	registry := tool.NewRegistry()
	for _, name := range []string{"bash", "edit", "read", "search_tools", "write"} {
		registry.Register(tool.Func(name, name, map[string]any{"type": "object"}, func(context.Context, string) (string, error) {
			return "", nil
		}))
	}
	registry.Register(tool.FuncWithMetadata(
		tool.RecallMemoryToolName,
		"recall workspace notes",
		map[string]any{"type": "object"},
		tool.Metadata{Category: "memory", ReadOnly: true, Concurrency: tool.Parallel},
		func(context.Context, string) (string, error) { return "", nil },
	))
	registry.Register(tool.FuncWithMetadata(
		tool.RememberMemoryToolName,
		"remember workspace note",
		map[string]any{"type": "object"},
		tool.Metadata{Category: "memory", Concurrency: tool.Serialized},
		func(context.Context, string) (string, error) { return "", nil },
	))

	if got := activeToolNamesForMode(registry, "read"); !slices.Equal(got, []string{
		"read", "search_tools", tool.RecallMemoryToolName,
	}) {
		t.Fatalf("read memory tools = %#v", got)
	}
	if got := activeToolNamesForMode(registry, "coding"); !slices.Equal(got, []string{
		"bash", "edit", "read", "search_tools", "write", tool.RecallMemoryToolName, tool.RememberMemoryToolName,
	}) {
		t.Fatalf("coding memory tools = %#v", got)
	}
}
