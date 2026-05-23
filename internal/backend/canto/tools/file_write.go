package tools

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"

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
		Parameters:  writeParameters(),
	}
}

func (w *Write) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[writeInput]("write", args)
	if err != nil {
		return "", err
	}

	absPath, err := w.mutationPath(input.FilePath)
	if err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", err
	}

	if _, err := w.checkpointPaths(ctx, input.FilePath); err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", err
	}

	if err := os.MkdirAll(filepath.Dir(absPath), 0o755); err != nil {
		return "", err
	}

	mode := os.FileMode(0o644)
	if info, err := os.Stat(absPath); err == nil {
		mode = info.Mode().Perm()
	} else if !errors.Is(err, os.ErrNotExist) {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", err
	}
	tmpPath, err := writeEditTempFile(absPath, []byte(input.Content), mode)
	if err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		_ = os.Remove(tmpPath)
		return "", err
	}
	if err := os.Rename(tmpPath, absPath); err != nil {
		_ = os.Remove(tmpPath)
		return "", err
	}

	return limitToolOutput(fmt.Sprintf("Wrote %s.", input.FilePath)), nil
}
