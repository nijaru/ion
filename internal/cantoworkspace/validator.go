package workspace

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"strings"
)

const DefaultMaxDepth = 64

var (
	ErrInvalidRoot   = errors.New("invalid workspace root")
	ErrInvalidPath   = errors.New("invalid workspace path")
	ErrAbsolutePath  = errors.New("absolute paths are not allowed")
	ErrPathTraversal = errors.New("path escapes workspace root")
	ErrPathTooDeep   = errors.New("path exceeds max depth")
)

// Validator applies strict path validation against one workspace root.
type Validator struct {
	base     string
	maxDepth int
}

// NewValidator creates a validator for rootPath. The root is normalized to an
// absolute symlink-resolved directory path.
func NewValidator(rootPath string) (*Validator, error) {
	if rootPath == "" {
		return nil, fmt.Errorf("%w: root path is required", ErrInvalidRoot)
	}

	abs, err := filepath.Abs(rootPath)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidRoot, err)
	}
	resolved, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidRoot, err)
	}
	info, err := os.Stat(resolved)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidRoot, err)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("%w: %q is not a directory", ErrInvalidRoot, resolved)
	}

	return &Validator{
		base:     resolved,
		maxDepth: DefaultMaxDepth,
	}, nil
}

// Base returns the canonical validated workspace root path.
func (v *Validator) Base() string {
	if v == nil {
		return ""
	}
	return v.base
}

// Validate canonicalizes and validates target as a path within the workspace.
func (v *Validator) Validate(target string) (string, error) {
	return v.validate(target, false)
}

func (v *Validator) validate(target string, allowRoot bool) (string, error) {
	if v == nil || v.base == "" {
		return "", fmt.Errorf("%w: validator is not configured", ErrInvalidRoot)
	}
	if target == "" {
		return "", fmt.Errorf("%w: path is required", ErrInvalidPath)
	}
	if strings.ContainsRune(target, '\x00') {
		return "", fmt.Errorf("%w: NUL byte in path %q", ErrInvalidPath, target)
	}
	if filepath.IsAbs(target) || filepath.VolumeName(target) != "" {
		return "", fmt.Errorf("%w: %q", ErrAbsolutePath, target)
	}

	cleaned := filepath.Clean(target)
	if cleaned == "." {
		if allowRoot {
			return ".", nil
		}
		return "", fmt.Errorf("%w: root directory reference %q", ErrInvalidPath, target)
	}
	if cleaned == ".." || strings.HasPrefix(cleaned, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("%w: %q", ErrPathTraversal, target)
	}
	if depth(cleaned) > v.maxDepth {
		return "", fmt.Errorf("%w: %q", ErrPathTooDeep, target)
	}

	candidate := filepath.Join(v.base, cleaned)
	if err := v.ensureContained(candidate); err != nil {
		return "", err
	}
	return cleaned, nil
}

func (v *Validator) ensureContained(candidate string) error {
	ancestor, err := nearestExistingAncestor(candidate)
	if err != nil {
		return fmt.Errorf("%w: %v", ErrInvalidPath, err)
	}
	resolved, err := filepath.EvalSymlinks(ancestor)
	if err != nil {
		return fmt.Errorf("%w: %v", ErrInvalidPath, err)
	}
	if !withinBase(v.base, resolved) {
		return fmt.Errorf("%w: %q resolves outside %q", ErrPathTraversal, candidate, v.base)
	}
	return nil
}

func nearestExistingAncestor(path string) (string, error) {
	current := path
	for {
		_, err := os.Lstat(current)
		if err == nil {
			return current, nil
		}
		if !errors.Is(err, fs.ErrNotExist) {
			return "", err
		}
		parent := filepath.Dir(current)
		if parent == current {
			return "", fs.ErrNotExist
		}
		current = parent
	}
}

func withinBase(base, candidate string) bool {
	rel, err := filepath.Rel(base, candidate)
	if err != nil {
		return false
	}
	return rel == "." || (rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)))
}

func depth(path string) int {
	if path == "." || path == "" {
		return 0
	}
	return len(strings.Split(path, string(filepath.Separator)))
}

func cleanGlobPattern(pattern string) (string, error) {
	pattern = strings.TrimSpace(pattern)
	if pattern == "" {
		return "", fmt.Errorf("%w: glob pattern is required", ErrInvalidPath)
	}
	if strings.ContainsRune(pattern, '\x00') {
		return "", fmt.Errorf("%w: NUL byte in glob pattern %q", ErrInvalidPath, pattern)
	}
	if filepath.IsAbs(pattern) || filepath.VolumeName(pattern) != "" {
		return "", fmt.Errorf("%w: %q", ErrAbsolutePath, pattern)
	}

	cleaned := path.Clean(filepath.ToSlash(pattern))
	if cleaned == "." {
		return "", fmt.Errorf("%w: root glob pattern %q", ErrInvalidPath, pattern)
	}
	if cleaned == ".." || strings.HasPrefix(cleaned, "../") {
		return "", fmt.Errorf("%w: %q", ErrPathTraversal, pattern)
	}
	if depth(filepath.FromSlash(cleaned)) > DefaultMaxDepth {
		return "", fmt.Errorf("%w: %q", ErrPathTooDeep, pattern)
	}
	return cleaned, nil
}
