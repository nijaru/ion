package tools

import (
	"context"
	"errors"
	"fmt"
	"os"
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
	if err := ctx.Err(); err != nil {
		return "", err
	}

	if _, err := w.checkpointPaths(ctx, input.FilePath); err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", err
	}

	if err := root.MkdirAll(filepath.Dir(relPath), 0o755); err != nil {
		return "", err
	}

	mode := os.FileMode(0o644)
	if info, err := root.Stat(relPath); err == nil {
		mode = info.Mode().Perm()
	} else if !errors.Is(err, os.ErrNotExist) {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", err
	}
	tmpPath, err := writeEditTempFile(root, relPath, []byte(input.Content), mode)
	if err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		_ = root.Remove(tmpPath)
		return "", err
	}
	if err := root.Rename(tmpPath, relPath); err != nil {
		_ = root.Remove(tmpPath)
		return "", err
	}

	return limitToolOutput(fmt.Sprintf("Wrote %s.", input.FilePath)), nil
}
