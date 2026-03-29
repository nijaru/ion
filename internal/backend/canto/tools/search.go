package tools

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/bmatcuk/doublestar/v4"
	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

type SearchTool struct {
	cwd string
}

func NewSearchTool(cwd string) *SearchTool {
	return &SearchTool{cwd: cwd}
}

// Grep tool
type Grep struct {
	SearchTool
}

func (g *Grep) Spec() llm.Spec {
	return llm.Spec{
		Name:        "grep",
		Description: "Search for a regex pattern in files using ripgrep.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"pattern": map[string]any{
					"type":        "string",
					"description": "The regex pattern to search for.",
				},
				"path": map[string]any{
					"type":        "string",
					"description": "The directory or file to search in (defaults to current working directory).",
				},
			},
			"required": []string{"pattern"},
		},
	}
}

func (g *Grep) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Pattern string `json:"pattern"`
		Path    string `json:"path"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	if input.Path == "" {
		input.Path = "."
	}

	// Try ripgrep first as it's the fastest
	cmd := exec.CommandContext(ctx, "rg", "--max-count", "100", "--heading", "--line-number", "--color", "never", input.Pattern, input.Path)
	cmd.Dir = g.cwd
	output, err := cmd.CombinedOutput()
	if err == nil {
		return string(output), nil
	}
	return "", fmt.Errorf("rg search failed: %w", err)
}

// Glob tool
type Glob struct {
	SearchTool
}

func (g *Glob) Spec() llm.Spec {
	return llm.Spec{
		Name:        "glob",
		Description: "Search for files matching a glob pattern (supports ** for recursive search).",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"pattern": map[string]any{
					"type":        "string",
					"description": "The glob pattern to search for (e.g. '**/*.go').",
				},
			},
			"required": []string{"pattern"},
		},
	}
}

func (g *Glob) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Pattern string `json:"pattern"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	// Use doublestar for robust, recursive globbing
	fsys := os.DirFS(g.cwd)
	matches, err := doublestar.Glob(fsys, input.Pattern)
	if err != nil {
		return "", fmt.Errorf("glob failed: %w", err)
	}

	if len(matches) == 0 {
		return "No matches found.", nil
	}

	return strings.Join(matches, "\n"), nil
}
