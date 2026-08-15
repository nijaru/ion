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
	Activate func(context.Context, []string) error
}

// NewSearchTool creates the framework search_tools meta-tool for a registry.
func NewSearchTool(reg *Registry) *SearchTool {
	return &SearchTool{Registry: reg}
}

// SetActivator connects discovery to the harness-owned active tool set.
// A nil activator keeps search as a read-only registry query.
func (s *SearchTool) SetActivator(activate func(context.Context, []string) error) {
	s.Activate = activate
}

func (s *SearchTool) Spec() llm.Spec {
	return llm.Spec{
		Name:        SearchToolName,
		Description: "Search available tools by capability, keyword, category, or exact name. Matching tools are activated and their specifications are returned for subsequent calls.",
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

func (s *SearchTool) Execute(ctx context.Context, args string) (string, error) {
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
	matchNames := make([]string, 0, len(entries))
	for _, entry := range entries {
		if entry.Name == SearchToolName {
			continue
		}
		if searchMatches(entry, query) {
			matches = append(matches, entry.Spec)
			matchNames = append(matchNames, entry.Name)
		}
	}
	if s.Activate != nil && len(matchNames) > 0 {
		if err := s.Activate(ctx, matchNames); err != nil {
			return "", err
		}
	}

	data, err := json.Marshal(matches)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

func searchMatches(entry ToolEntry, query string) bool {
	q := strings.ToLower(strings.TrimSpace(query))
	if q == "" {
		return false
	}
	target := strings.ToLower(entry.Name + " " + entry.Spec.Description + " " + entry.Metadata.Category)
	for _, example := range entry.Metadata.Examples {
		target += " " + strings.ToLower(example.Description)
	}
	if strings.Contains(target, q) {
		return true
	}
	tokens := strings.Fields(q)
	if len(tokens) <= 1 {
		return false
	}
	for _, tok := range tokens {
		if !strings.Contains(target, tok) {
			return false
		}
	}
	return true
}
