package tools

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

// List tool (formerly list_directory)
type List struct {
	FileTool
}

func (l *List) Spec() llm.Spec {
	return llm.Spec{
		Name:        "list",
		Description: "List contents of a specific directory.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"path": map[string]any{
					"type":        "string",
					"description": "Directory to list (default: current directory).",
				},
			},
		},
	}
}

func (l *List) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Path string `json:"path"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	if input.Path == "" {
		input.Path = "."
	}

	absPath, err := l.absolutePath(input.Path)
	if err != nil {
		return "", err
	}

	entries, err := os.ReadDir(absPath)
	if err != nil {
		return "", err
	}

	var res strings.Builder
	for _, e := range entries {
		suffix := ""
		if e.IsDir() {
			suffix = "/"
		}
		res.WriteString(fmt.Sprintf("%s%s\n", e.Name(), suffix))
	}
	return limitToolOutput(res.String()), nil
}
