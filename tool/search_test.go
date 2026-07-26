package tool

import (
	"context"
	"testing"
)

func TestSearchTool_ActivatesMatches(t *testing.T) {
	reg := NewRegistry()
	reg.Register(
		Func(
			"grep_workspace",
			"Search files",
			map[string]any{"type": "object"},
			func(context.Context, string) (string, error) {
				return "", nil
			},
		),
	)
	var activated []string
	search := NewSearchTool(reg)
	search.SetActivator(func(_ context.Context, names []string) error {
		activated = append(activated, names...)
		return nil
	})

	if _, err := search.Execute(t.Context(), `{"query":"workspace"}`); err != nil {
		t.Fatal(err)
	}
	if len(activated) != 1 || activated[0] != "grep_workspace" {
		t.Fatalf("activated tools = %#v", activated)
	}
}

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
