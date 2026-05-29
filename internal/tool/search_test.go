package tool

import (
	"context"
	"testing"
)

func TestSearchTool_UsesMetadataInMatching(t *testing.T) {
	reg := NewRegistry()
	reg.Register(FuncWithMetadata(
		"grep_workspace",
		"Search files",
		map[string]any{"type": "object"},
		Metadata{
			Category: "workspace",
			Examples: []Example{{Description: "search code in files"}},
		},
		func(context.Context, string) (string, error) { return "", nil },
	))

	got, err := NewSearchTool(reg).Execute(t.Context(), `{"query":"workspace"}`)
	if err != nil {
		t.Fatalf("execute search tool: %v", err)
	}
	if got == "[]" {
		t.Fatalf("expected workspace category match, got %s", got)
	}
}
