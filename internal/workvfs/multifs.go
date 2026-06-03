package workspace

import (
	"context"
	"io/fs"
	"os"
	"path"
	"slices"
	"strings"
	"time"
)

// MultiFS allows mounting multiple WorkspaceFS implementations at different
// rooted paths.
type MultiFS struct {
	base   WorkspaceFS
	mounts map[string]WorkspaceFS
}

// NewMultiFS creates a new MultiFS wrapping a base filesystem.
func NewMultiFS(base WorkspaceFS) *MultiFS {
	return &MultiFS{
		base:   base,
		mounts: make(map[string]WorkspaceFS),
	}
}

// Mount attaches a filesystem at the given rooted path.
func (m *MultiFS) Mount(path string, fs WorkspaceFS) {
	path = strings.Trim(path, "/")
	if path == "" {
		m.base = fs
		return
	}
	m.mounts[path] = fs
}

func (m *MultiFS) Path() string {
	return m.base.Path()
}

func (m *MultiFS) Close() error {
	err := m.base.Close()
	for _, fs := range m.mounts {
		if closeErr := fs.Close(); closeErr != nil {
			err = closeErr
		}
	}
	return err
}

func (m *MultiFS) FS() fs.FS {
	return m.base.FS()
}

func (m *MultiFS) MkdirAll(name string, perm os.FileMode) error {
	fs, sub, ok := m.resolve(name)
	if ok {
		return fs.MkdirAll(sub, perm)
	}
	return m.base.MkdirAll(name, perm)
}

func (m *MultiFS) ReadFile(name string) ([]byte, error) {
	fs, sub, ok := m.resolve(name)
	if ok {
		return fs.ReadFile(sub)
	}
	return m.base.ReadFile(name)
}

func (m *MultiFS) WriteFile(name string, data []byte, perm os.FileMode) error {
	fs, sub, ok := m.resolve(name)
	if ok {
		return fs.WriteFile(sub, data, perm)
	}
	return m.base.WriteFile(name, data, perm)
}

func (m *MultiFS) Remove(name string) error {
	fs, sub, ok := m.resolve(name)
	if ok {
		return fs.Remove(sub)
	}
	return m.base.Remove(name)
}

func (m *MultiFS) ReadDir(name string) ([]fs.DirEntry, error) {
	mounted, sub, ok := m.resolve(name)
	if ok {
		return mounted.ReadDir(sub)
	}
	entries, err := m.base.ReadDir(name)
	if name == "." || name == "" {
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		for mount := range m.mounts {
			entries = append(entries, virtualDirEntry{name: mount})
		}
		slices.SortFunc(entries, func(a, b fs.DirEntry) int {
			return strings.Compare(a.Name(), b.Name())
		})
		return entries, nil
	}
	return entries, err
}

func (m *MultiFS) Stat(name string) (fs.FileInfo, error) {
	fs, sub, ok := m.resolve(name)
	if ok {
		return fs.Stat(sub)
	}
	return m.base.Stat(name)
}

func (m *MultiFS) Glob(ctx context.Context, pattern string) ([]string, error) {
	pattern, err := cleanGlobPattern(pattern)
	if err != nil {
		return nil, err
	}
	baseMatches, err := m.base.Glob(ctx, pattern)
	if err != nil {
		return nil, err
	}

	matchSet := make(map[string]struct{}, len(baseMatches)+len(m.mounts))
	for _, match := range baseMatches {
		matchSet[match] = struct{}{}
	}
	for mount, mounted := range m.mounts {
		matched, err := path.Match(pattern, mount)
		if err != nil {
			return nil, err
		}
		if matched {
			matchSet[mount] = struct{}{}
		}
		if !strings.HasPrefix(pattern, mount+"/") {
			continue
		}
		subPattern := strings.TrimPrefix(pattern, mount+"/")
		mountedMatches, err := mounted.Glob(ctx, subPattern)
		if err != nil {
			return nil, err
		}
		for _, match := range mountedMatches {
			matchSet[path.Join(mount, match)] = struct{}{}
		}
	}

	matches := make([]string, 0, len(matchSet))
	for match := range matchSet {
		matches = append(matches, match)
	}
	slices.Sort(matches)
	return matches, nil
}

func (m *MultiFS) resolve(name string) (WorkspaceFS, string, bool) {
	name = strings.TrimPrefix(path.Clean(name), "/")
	for mount, fs := range m.mounts {
		if name == mount {
			return fs, ".", true
		}
		if strings.HasPrefix(name, mount+"/") {
			return fs, strings.TrimPrefix(name, mount+"/"), true
		}
	}
	return nil, "", false
}

type virtualDirEntry struct {
	name string
}

func (e virtualDirEntry) Name() string               { return e.name }
func (e virtualDirEntry) IsDir() bool                { return true }
func (e virtualDirEntry) Type() fs.FileMode          { return os.ModeDir }
func (e virtualDirEntry) Info() (fs.FileInfo, error) { return virtualFileInfo{name: e.name}, nil }

type virtualFileInfo struct {
	name string
}

func (i virtualFileInfo) Name() string       { return i.name }
func (i virtualFileInfo) Size() int64        { return 0 }
func (i virtualFileInfo) Mode() os.FileMode  { return os.ModeDir }
func (i virtualFileInfo) ModTime() time.Time { return time.Time{} }
func (i virtualFileInfo) IsDir() bool        { return true }
func (i virtualFileInfo) Sys() any           { return nil }
