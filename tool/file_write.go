package tool

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"github.com/nijaru/ion/llm"
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

func (w *Write) ApprovalRequirement(args string) (Requirement, bool, error) {
	input, err := decodeToolArgs[writeInput]("write", args)
	if err != nil {
		return Requirement{}, false, err
	}
	return Requirement{Category: "write", Operation: "write", Resource: input.Path}, true, nil
}

func (w *Write) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[writeInput]("write", args)
	if err != nil {
		return "", err
	}
	return WithFileMutationQueue(input.Path, func() (string, error) {
		return w.execute(ctx, args)
	})
}

func (w *Write) execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[writeInput]("write", args)
	if err != nil {
		return "", err
	}

	absPath, err := w.mutationPath(input.Path)
	if err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", toolContextErr("write", err)
	}

	if _, err := w.checkpointPaths(ctx, input.Path); err != nil {
		return "", toolContextErr("write", err)
	}
	if err := ctx.Err(); err != nil {
		return "", toolContextErr("write", err)
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
		return "", toolContextErr("write", err)
	}
	if err := replaceFile(ctx, "write", absPath, []byte(input.Content), mode); err != nil {
		return "", err
	}

	return limitToolOutput(fmt.Sprintf("Wrote %s.", input.Path)), nil
}
