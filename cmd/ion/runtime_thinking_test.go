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
