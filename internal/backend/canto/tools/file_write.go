package tools

import (
	"context"
	"fmt"
	"path/filepath"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

// Write tool (formerly write_file)
type Write struct {
	FileTool
}

func (w *Write) Spec() llm.Spec {
	return llm.Spec{
		Name:        "write",
		Description: "Create or overwrite a file with new content. Use for new files or complete rewrites.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"file_path": map[string]any{
					"type":        "string",
					"description": "Relative path to the file.",
				},
				"content": map[string]any{
					"type":        "string",
					"description": "The full content to write to the file.",
				},
			},
			"required": []string{"file_path", "content"},
		},
	}
}

func (w *Write) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		FilePath string `json:"file_path"`
		Content  string `json:"content"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	relPath, err := w.relativePath(input.FilePath)
	if err != nil {
		return "", err
	}
	root, err := w.openRoot()
	if err != nil {
		return "", err
	}
	defer root.Close()

	checkpointID, err := w.checkpointPaths(ctx, input.FilePath)
	if err != nil {
		return "", err
	}

	if err := root.MkdirAll(filepath.Dir(relPath), 0o755); err != nil {
		return "", err
	}

	if err := root.WriteFile(relPath, []byte(input.Content), 0o644); err != nil {
		return "", err
	}
	return limitToolOutput(
		appendCheckpointID(
			fmt.Sprintf("Successfully wrote %d bytes to %s", len(input.Content), input.FilePath),
			checkpointID,
		),
	), nil
}
