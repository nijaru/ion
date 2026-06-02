package tool

import (
	"context"
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/ion/llm"
)

const SearchToolName = "search_tools"

// SearchTool lets the model discover deferred or hidden tools without
// injecting the full registry into every request.
type SearchTool struct {
	Registry *Registry
}

// NewSearchTool creates the framework search_tools meta-tool for a registry.
func NewSearchTool(reg *Registry) *SearchTool {
	return &SearchTool{Registry: reg}
}

func (s *SearchTool) Spec() llm.Spec {
	return llm.Spec{
		Name:        SearchToolName,
		Description: "Search available tools by capability, keyword, category, or exact name. Returns matching tool specifications so you can call them in later tool uses.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"query": map[string]any{
					"type":        "string",
					"description": "Capability, keyword, category, or tool name to search for.",
				},
			},
			"required": []string{"query"},
		},
	}
}

func (s *SearchTool) Metadata() Metadata {
	return Metadata{
		Category:    "meta",
		ReadOnly:    true,
		Concurrency: Parallel,
	}
}

func (s *SearchTool) Execute(_ context.Context, args string) (string, error) {
	var input struct {
		Query string `json:"query"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	query := strings.TrimSpace(strings.ToLower(input.Query))
	if query == "" {
		return "[]", nil
	}

	entries := s.Registry.Entries()
	matches := make([]llm.Spec, 0, len(entries))
	for _, entry := range entries {
		if entry.Name == SearchToolName {
			continue
		}
		if searchMatches(entry, query) {
			matches = append(matches, entry.Spec)
		}
	}

	data, err := json.Marshal(matches)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

func searchMatches(entry ToolEntry, query string) bool {
	if strings.Contains(strings.ToLower(entry.Name), query) {
		return true
	}
	if strings.Contains(strings.ToLower(entry.Spec.Description), query) {
		return true
	}
	if strings.Contains(strings.ToLower(entry.Metadata.Category), query) {
		return true
	}
	for _, example := range entry.Metadata.Examples {
		if strings.Contains(strings.ToLower(example.Description), query) {
			return true
		}
	}
	return false
}
