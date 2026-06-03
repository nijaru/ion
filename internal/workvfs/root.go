package workspace

import (
	"context"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/bmatcuk/doublestar/v4"
)

// Root provides symlink-safe rooted filesystem access for one workspace.
type Root struct {
	path      string
	root      *os.Root
	validator *Validator
}

// Open opens path as a rooted workspace.
func Open(path string) (*Root, error) {
	validator, err := NewValidator(path)
	if err != nil {
		return nil, fmt.Errorf("open workspace: %w", err)
	}
	root, err := os.OpenRoot(validator.base)
	if err != nil {
		return nil, fmt.Errorf("open workspace: %w", err)
	}
	return &Root{path: validator.base, root: root, validator: validator}, nil
}

// Path returns the absolute path of the opened workspace root.
func (r *Root) Path() string {
	if r == nil {
		return ""
	}
	return r.path
}

// Close releases the underlying rooted handle.
func (r *Root) Close() error {
	if r == nil || r.root == nil {
		return nil
	}
	return r.root.Close()
}

// FS exposes the rooted filesystem as an fs.FS.
func (r *Root) FS() fs.FS {
	if r == nil || r.root == nil {
		return nil
	}
	return r.root.FS()
}

// Open opens a file relative to the workspace root.
func (r *Root) Open(name string) (*os.File, error) {
	if r == nil || r.root == nil {
		return nil, fmt.Errorf("workspace is not open")
	}
	name, err := r.validate(name, false)
	if err != nil {
		return nil, err
	}
	return r.root.Open(name)
}

// OpenFile opens a file relative to the workspace root with flags and mode.
func (r *Root) OpenFile(name string, flag int, perm os.FileMode) (*os.File, error) {
	if r == nil || r.root == nil {
		return nil, fmt.Errorf("workspace is not open")
	}
	var err error
	name, err = r.validate(name, false)
	if err != nil {
		return nil, err
	}
	return r.root.OpenFile(name, flag, perm)
}

// MkdirAll creates path relative to the workspace root.
func (r *Root) MkdirAll(path string, perm os.FileMode) error {
	if r == nil || r.root == nil {
		return fmt.Errorf("workspace is not open")
	}
	var err error
	path, err = r.validate(path, true)
	if err != nil {
		return err
	}
	return r.root.MkdirAll(path, perm)
}

// ReadFile reads a file relative to the workspace root.
func (r *Root) ReadFile(name string) ([]byte, error) {
	if r == nil || r.root == nil {
		return nil, fmt.Errorf("workspace is not open")
	}
	var err error
	name, err = r.validate(name, false)
	if err != nil {
		return nil, err
	}
	return r.root.ReadFile(name)
}

// Stat returns file metadata relative to the workspace root.
func (r *Root) Stat(name string) (fs.FileInfo, error) {
	if r == nil || r.root == nil {
		return nil, fmt.Errorf("workspace is not open")
	}
	var err error
	name, err = r.validate(name, true)
	if err != nil {
		return nil, err
	}
	return r.root.Stat(name)
}

// WriteFile writes a file relative to the workspace root, creating parent
// directories inside the workspace if needed.
func (r *Root) WriteFile(name string, data []byte, perm os.FileMode) error {
	if r == nil || r.root == nil {
		return fmt.Errorf("workspace is not open")
	}
	var err error
	name, err = r.validate(name, false)
	if err != nil {
		return err
	}
	dir := filepath.Dir(name)
	if dir != "." {
		if err := r.root.MkdirAll(dir, 0o755); err != nil {
			return err
		}
	}
	return r.root.WriteFile(name, data, perm)
}

// Remove deletes a file or empty directory relative to the workspace root.
func (r *Root) Remove(name string) error {
	if r == nil || r.root == nil {
		return fmt.Errorf("workspace is not open")
	}
	var err error
	name, err = r.validate(name, false)
	if err != nil {
		return err
	}
	return r.root.Remove(name)
}

// ReadDir lists directory entries relative to the workspace root.

func (r *Root) ReadDir(name string) ([]fs.DirEntry, error) {
	if r == nil || r.root == nil {
		return nil, fmt.Errorf("workspace is not open")
	}
	var err error
	name, err = r.validate(name, true)
	if err != nil {
		return nil, err
	}
	f, err := r.root.Open(name)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	return f.ReadDir(-1)
}

// Glob walks the rooted filesystem and returns files matching pattern.
func (r *Root) Glob(ctx context.Context, pattern string) ([]string, error) {
	if r == nil || r.root == nil {
		return nil, fmt.Errorf("workspace is not open")
	}
	pattern, err := cleanGlobPattern(pattern)
	if err != nil {
		return nil, err
	}

	var matches []string
	err = fs.WalkDir(r.root.FS(), ".", func(path string, d fs.DirEntry, err error) error {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		if err != nil {
			return nil
		}
		if d.IsDir() {
			return nil
		}
		ok, matchErr := doublestar.Match(pattern, filepath.ToSlash(path))
		if matchErr != nil {
			return fmt.Errorf("glob: invalid pattern %q: %w", pattern, matchErr)
		}
		if ok {
			matches = append(matches, path)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return matches, nil
}

func (r *Root) validate(name string, allowRoot bool) (string, error) {
	if r == nil || r.validator == nil {
		return "", fmt.Errorf("workspace is not open")
	}
	return r.validator.validate(name, allowRoot)
}
