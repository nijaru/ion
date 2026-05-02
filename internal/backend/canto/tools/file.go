package tools

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

type FileTool struct {
	cwd        string
	checkpoint *ionworkspace.CheckpointStore
}

func NewFileTool(cwd string) *FileTool {
	path, err := ionworkspace.DefaultCheckpointPath()
	if err != nil {
		return &FileTool{cwd: cwd}
	}
	return &FileTool{cwd: cwd, checkpoint: ionworkspace.NewCheckpointStore(path)}
}

func (t *FileTool) openRoot() (*os.Root, error) {
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return nil, err
	}
	return os.OpenRoot(absCwd)
}

func (t *FileTool) relativePath(target string) (string, error) {
	target = strings.TrimSpace(target)
	if target == "" {
		return "", fmt.Errorf("path is required")
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}

	if filepath.IsAbs(target) {
		absPath, err := filepath.Abs(target)
		if err != nil {
			return "", err
		}
		target, err = filepath.Rel(absCwd, absPath)
		if err != nil {
			return "", err
		}
	}

	target = filepath.Clean(target)
	if !filepath.IsLocal(target) {
		return "", fmt.Errorf("path escapes workspace: %s", target)
	}
	return target, nil
}

// resolvePath returns the lexical absolute path for display/diff metadata only.
// File operations use os.Root methods so symlinks cannot escape the workspace.
func (t *FileTool) resolvePath(target string) (string, error) {
	relPath, err := t.relativePath(target)
	if err != nil {
		return "", err
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	return filepath.Join(absCwd, relPath), nil
}

func (t *FileTool) checkpointPaths(ctx context.Context, paths ...string) (string, error) {
	if t.checkpoint == nil {
		return "", nil
	}
	relPaths := make([]string, 0, len(paths))
	for _, path := range paths {
		relPath, err := t.relativePath(path)
		if err != nil {
			return "", err
		}
		relPaths = append(relPaths, relPath)
	}
	cp, err := t.checkpoint.Create(ctx, t.cwd, relPaths)
	if err != nil {
		return "", err
	}
	return cp.ID, nil
}

func appendCheckpointID(message, id string) string {
	if id == "" {
		return message
	}
	return message + "\nCheckpoint: " + id
}
