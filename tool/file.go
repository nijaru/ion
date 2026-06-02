package tool

import (
	"context"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	ionworkspace "github.com/nijaru/ion/internal/workspace"
	"golang.org/x/text/unicode/norm"
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

func (t *FileTool) absolutePath(target string) (string, error) {
	target, err := normalizeToolPathInput(target)
	if err != nil {
		return "", err
	}
	if target == "" {
		return "", fmt.Errorf("path is required")
	}
	target, err = expandHomePath(target)
	if err != nil {
		return "", err
	}
	if filepath.IsAbs(target) {
		return filepath.Clean(target), nil
	}

	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	return filepath.Clean(filepath.Join(absCwd, target)), nil
}

func normalizeToolPathInput(target string) (string, error) {
	target = normalizeUnicodeSpaces(strings.TrimSpace(target))
	if strings.HasPrefix(target, "@") {
		target = strings.TrimPrefix(target, "@")
	}
	if !strings.HasPrefix(target, "file://") {
		return target, nil
	}
	u, err := url.Parse(target)
	if err != nil {
		return "", fmt.Errorf("parse file URL: %w", err)
	}
	if u.Scheme != "file" {
		return target, nil
	}
	if u.Host != "" && u.Host != "localhost" {
		return "", fmt.Errorf("unsupported file URL host: %s", u.Host)
	}
	return u.Path, nil
}

func normalizeUnicodeSpaces(target string) string {
	return strings.Map(func(r rune) rune {
		switch {
		case r == '\u00a0' ||
			r == '\u202f' ||
			r == '\u205f' ||
			r == '\u3000' ||
			(r >= '\u2000' && r <= '\u200a'):
			return ' '
		default:
			return r
		}
	}, target)
}

func expandHomePath(target string) (string, error) {
	if target == "~" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		return home, nil
	}
	prefix := "~" + string(filepath.Separator)
	if strings.HasPrefix(target, prefix) {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		return filepath.Join(home, strings.TrimPrefix(target, prefix)), nil
	}
	return target, nil
}

func (t *FileTool) relativePath(target string) (string, error) {
	absPath, err := t.absolutePath(target)
	if err != nil {
		return "", err
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	relPath, err := filepath.Rel(absCwd, absPath)
	if err != nil {
		return "", err
	}
	return filepath.Clean(relPath), nil
}

func (t *FileTool) workspaceRelativePath(target string) (string, bool, error) {
	relPath, err := t.relativePath(target)
	if err != nil {
		return "", false, err
	}
	if relPath == "." || filepath.IsLocal(relPath) {
		return relPath, true, nil
	}
	return "", false, nil
}

func (t *FileTool) resolvePath(target string) (string, error) {
	return t.absolutePath(target)
}

func (t *FileTool) readPath(target string) (string, error) {
	absPath, err := t.absolutePath(target)
	if err != nil {
		return "", err
	}
	if fileExists(absPath) {
		return absPath, nil
	}
	for _, candidate := range readPathVariants(absPath) {
		if candidate != absPath && fileExists(candidate) {
			return candidate, nil
		}
	}
	return absPath, nil
}

func (t *FileTool) mutationPath(target string) (string, error) {
	absPath, err := t.absolutePath(target)
	if err != nil {
		return "", err
	}
	return realPathForPossiblyMissingPath(absPath)
}

func (t *FileTool) checkpointPaths(ctx context.Context, paths ...string) (string, error) {
	if t.checkpoint == nil {
		return "", nil
	}
	relPaths := make([]string, 0, len(paths))
	for _, path := range paths {
		relPath, ok, err := t.checkpointPath(path)
		if err != nil {
			return "", err
		}
		if !ok {
			continue
		}
		relPaths = append(relPaths, relPath)
	}
	if len(relPaths) == 0 {
		return "", nil
	}
	cp, err := t.checkpoint.Create(ctx, t.cwd, relPaths)
	if err != nil {
		return "", err
	}
	return cp.ID, nil
}

func (t *FileTool) checkpointPath(target string) (string, bool, error) {
	relPath, ok, err := t.workspaceRelativePath(target)
	if err != nil || !ok {
		return "", ok, err
	}
	inside, err := t.realPathInsideWorkspace(target)
	if err != nil {
		return "", false, err
	}
	if !inside {
		return "", false, nil
	}
	return relPath, true, nil
}

func (t *FileTool) realPathInsideWorkspace(target string) (bool, error) {
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return false, err
	}
	realCwd, err := filepath.EvalSymlinks(absCwd)
	if err != nil {
		realCwd = absCwd
	}
	absPath, err := t.absolutePath(target)
	if err != nil {
		return false, err
	}
	realPath, err := realPathForPossiblyMissingPath(absPath)
	if err != nil {
		return false, err
	}
	rel, err := filepath.Rel(realCwd, realPath)
	if err != nil {
		return false, nil
	}
	return rel == "." || filepath.IsLocal(rel), nil
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

var macOSScreenshotAMPM = regexp.MustCompile(` (?i:am|pm)\.`)

func readPathVariants(path string) []string {
	nfd := norm.NFD.String(path)
	return []string{
		tryMacOSScreenshotPath(path),
		nfd,
		strings.ReplaceAll(path, "'", "\u2019"),
		strings.ReplaceAll(nfd, "'", "\u2019"),
	}
}

func tryMacOSScreenshotPath(path string) string {
	return macOSScreenshotAMPM.ReplaceAllStringFunc(path, func(match string) string {
		return "\u202f" + match[1:]
	})
}

func realPathForPossiblyMissingPath(path string) (string, error) {
	if realPath, err := filepath.EvalSymlinks(path); err == nil {
		return realPath, nil
	}

	var suffix []string
	probe := path
	for {
		parent := filepath.Dir(probe)
		if parent == probe {
			return filepath.Clean(path), nil
		}
		suffix = append([]string{filepath.Base(probe)}, suffix...)
		probe = parent
		realParent, err := filepath.EvalSymlinks(probe)
		if err == nil {
			parts := append([]string{realParent}, suffix...)
			return filepath.Join(parts...), nil
		}
	}
}
