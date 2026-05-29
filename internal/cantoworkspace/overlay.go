package workspace

import (
	"context"
	"fmt"
	"io/fs"
	"os"
	"path"
	"slices"
	"strings"
	"sync"
	"time"
)

// OverlayFS implements a speculative virtual filesystem layer over a base
// workspace. Writes are buffered in memory and can be committed or discarded.
type OverlayFS struct {
	mu          sync.RWMutex
	base        WorkspaceFS
	speculative map[string]*overlayFile
	deleted     map[string]struct{}
}

type overlayFile struct {
	name    string
	data    []byte
	perm    os.FileMode
	modTime time.Time
	isDir   bool
}

// NewOverlayFS creates a new speculative overlay over a base filesystem.
func NewOverlayFS(base WorkspaceFS) *OverlayFS {
	return &OverlayFS{
		base:        base,
		speculative: make(map[string]*overlayFile),
		deleted:     make(map[string]struct{}),
	}
}

func (o *OverlayFS) Path() string { return o.base.Path() }
func (o *OverlayFS) Close() error { return o.base.Close() }
func (o *OverlayFS) FS() fs.FS    { return overlayFSView{overlay: o} }

func (o *OverlayFS) ensureParents(name string) {
	dir := path.Dir(name)
	for dir != "." && dir != "/" {
		if _, ok := o.speculative[dir]; !ok {
			o.speculative[dir] = &overlayFile{
				name:    path.Base(dir),
				perm:    0o755,
				modTime: time.Now(),
				isDir:   true,
			}
			delete(o.deleted, dir)
		}
		dir = path.Dir(dir)
	}
}

func (o *OverlayFS) MkdirAll(name string, perm os.FileMode) error {
	name, err := cleanOverlayPath(name, true)
	if err != nil {
		return err
	}

	o.mu.Lock()
	defer o.mu.Unlock()

	o.ensureParents(name)
	o.speculative[name] = &overlayFile{
		name:    path.Base(name),
		perm:    perm,
		modTime: time.Now(),
		isDir:   true,
	}
	delete(o.deleted, name)
	return nil
}

func (o *OverlayFS) ReadFile(name string) ([]byte, error) {
	name, err := cleanOverlayPath(name, false)
	if err != nil {
		return nil, err
	}

	o.mu.RLock()
	defer o.mu.RUnlock()

	if _, ok := o.deleted[name]; ok {
		return nil, os.ErrNotExist
	}
	if f, ok := o.speculative[name]; ok {
		if f.isDir {
			return nil, fmt.Errorf("read: %s is a directory", name)
		}
		return slices.Clone(f.data), nil
	}
	return o.base.ReadFile(name)
}

func (o *OverlayFS) WriteFile(name string, data []byte, perm os.FileMode) error {
	name, err := cleanOverlayPath(name, false)
	if err != nil {
		return err
	}

	o.mu.Lock()
	defer o.mu.Unlock()

	o.ensureParents(name)
	o.speculative[name] = &overlayFile{
		name:    path.Base(name),
		data:    slices.Clone(data),
		perm:    perm,
		modTime: time.Now(),
		isDir:   false,
	}
	delete(o.deleted, name)
	return nil
}

func (o *OverlayFS) Remove(name string) error {
	name, err := cleanOverlayPath(name, false)
	if err != nil {
		return err
	}

	o.mu.Lock()
	defer o.mu.Unlock()

	o.deleted[name] = struct{}{}
	delete(o.speculative, name)
	return nil
}

func (o *OverlayFS) ReadDir(name string) ([]fs.DirEntry, error) {
	name, err := cleanOverlayPath(name, true)
	if err != nil {
		return nil, err
	}

	o.mu.RLock()
	baseEntries, err := o.base.ReadDir(name)
	baseMissing := os.IsNotExist(err)
	if err != nil && !baseMissing {
		o.mu.RUnlock()
		return nil, err
	}
	spec := make(map[string]*overlayFile, len(o.speculative))
	for key, value := range o.speculative {
		spec[key] = value
	}
	deleted := make(map[string]struct{}, len(o.deleted))
	for key := range o.deleted {
		deleted[key] = struct{}{}
	}
	o.mu.RUnlock()
	if _, ok := deleted[name]; ok {
		return nil, os.ErrNotExist
	}

	entryMap := make(map[string]fs.DirEntry)
	for _, e := range baseEntries {
		p := path.Join(name, e.Name())
		if _, ok := deleted[p]; !ok {
			entryMap[e.Name()] = e
		}
	}

	for p, f := range spec {
		if path.Dir(p) == name {
			entryMap[f.name] = &overlayDirEntry{f: f}
		}
	}
	if baseMissing && len(entryMap) == 0 {
		if f, ok := spec[name]; !ok || !f.isDir {
			return nil, os.ErrNotExist
		}
	}

	entries := make([]fs.DirEntry, 0, len(entryMap))
	for _, e := range entryMap {
		entries = append(entries, e)
	}
	slices.SortFunc(entries, func(a, b fs.DirEntry) int {
		return strings.Compare(a.Name(), b.Name())
	})
	return entries, nil
}

func (o *OverlayFS) Stat(name string) (fs.FileInfo, error) {
	name, err := cleanOverlayPath(name, true)
	if err != nil {
		return nil, err
	}

	o.mu.RLock()
	defer o.mu.RUnlock()

	if _, ok := o.deleted[name]; ok {
		return nil, os.ErrNotExist
	}
	if f, ok := o.speculative[name]; ok {
		return &overlayFileInfo{f: f}, nil
	}
	return o.base.Stat(name)
}

func (o *OverlayFS) Glob(ctx context.Context, pattern string) ([]string, error) {
	pattern, err := cleanOverlayPath(pattern, false)
	if err != nil {
		return nil, err
	}

	baseMatches, err := o.base.Glob(ctx, pattern)
	if err != nil {
		return nil, err
	}

	o.mu.RLock()
	spec := make(map[string]*overlayFile, len(o.speculative))
	for key, value := range o.speculative {
		spec[key] = value
	}
	deleted := make(map[string]struct{}, len(o.deleted))
	for key := range o.deleted {
		deleted[key] = struct{}{}
	}
	o.mu.RUnlock()

	matchSet := make(map[string]struct{}, len(baseMatches)+len(spec))
	for _, match := range baseMatches {
		if _, ok := deleted[path.Clean(match)]; !ok {
			matchSet[match] = struct{}{}
		}
	}
	for name := range spec {
		if _, ok := deleted[name]; ok {
			continue
		}
		matched, err := path.Match(pattern, name)
		if err != nil {
			return nil, err
		}
		if matched {
			matchSet[name] = struct{}{}
		}
	}

	matches := make([]string, 0, len(matchSet))
	for match := range matchSet {
		matches = append(matches, match)
	}
	slices.Sort(matches)
	return matches, nil
}

func cleanOverlayPath(name string, allowRoot bool) (string, error) {
	if name == "" {
		return "", fmt.Errorf("%w: path is required", ErrInvalidPath)
	}
	if strings.ContainsRune(name, '\x00') {
		return "", fmt.Errorf("%w: NUL byte in path %q", ErrInvalidPath, name)
	}
	if path.IsAbs(name) {
		return "", fmt.Errorf("%w: %q", ErrAbsolutePath, name)
	}
	cleaned := path.Clean(name)
	if cleaned == "." {
		if allowRoot {
			return ".", nil
		}
		return "", fmt.Errorf("%w: root directory reference %q", ErrInvalidPath, name)
	}
	if cleaned == ".." || strings.HasPrefix(cleaned, "../") {
		return "", fmt.Errorf("%w: %q", ErrPathTraversal, name)
	}
	return cleaned, nil
}

// Commit applies all speculative changes to the base filesystem.
func (o *OverlayFS) Commit(ctx context.Context) error {
	o.mu.Lock()
	defer o.mu.Unlock()

	deletedPaths := make([]string, 0, len(o.deleted))
	for p := range o.deleted {
		deletedPaths = append(deletedPaths, p)
	}
	slices.SortFunc(deletedPaths, func(a, b string) int {
		return strings.Compare(b, a)
	})
	for _, p := range deletedPaths {
		if err := o.base.Remove(p); err != nil && !os.IsNotExist(err) {
			return err
		}
	}

	specPaths := make([]string, 0, len(o.speculative))
	for p := range o.speculative {
		specPaths = append(specPaths, p)
	}
	slices.SortFunc(specPaths, strings.Compare)

	for _, p := range specPaths {
		f := o.speculative[p]
		if f.isDir {
			if err := o.base.MkdirAll(p, f.perm); err != nil {
				return err
			}
		} else {
			if err := o.base.WriteFile(p, f.data, f.perm); err != nil {
				return err
			}
		}
	}

	o.speculative = make(map[string]*overlayFile)
	o.deleted = make(map[string]struct{})
	return nil
}

// Discard clears all speculative changes.
func (o *OverlayFS) Discard() {
	o.mu.Lock()
	defer o.mu.Unlock()
	o.speculative = make(map[string]*overlayFile)
	o.deleted = make(map[string]struct{})
}

// OverlaySnapshot is a point-in-time capture of the speculative overlay state.
type OverlaySnapshot struct {
	speculative map[string]*overlayFile
	deleted     map[string]struct{}
}

// Snapshot captures the current speculative state.
func (o *OverlayFS) Snapshot() *OverlaySnapshot {
	o.mu.RLock()
	defer o.mu.RUnlock()

	spec := make(map[string]*overlayFile, len(o.speculative))
	for k, v := range o.speculative {
		spec[k] = v
	}
	deleted := make(map[string]struct{}, len(o.deleted))
	for k := range o.deleted {
		deleted[k] = struct{}{}
	}
	return &OverlaySnapshot{speculative: spec, deleted: deleted}
}

// RestoreSnapshot replaces the current speculative state with a previous snapshot.
func (o *OverlayFS) RestoreSnapshot(s *OverlaySnapshot) {
	if s == nil {
		o.Discard()
		return
	}

	o.mu.Lock()
	defer o.mu.Unlock()

	spec := make(map[string]*overlayFile, len(s.speculative))
	for k, v := range s.speculative {
		spec[k] = v
	}
	deleted := make(map[string]struct{}, len(s.deleted))
	for k := range s.deleted {
		deleted[k] = struct{}{}
	}
	o.speculative = spec
	o.deleted = deleted
}

type overlayDirEntry struct {
	f *overlayFile
}

func (e *overlayDirEntry) Name() string               { return e.f.name }
func (e *overlayDirEntry) IsDir() bool                { return e.f.isDir }
func (e *overlayDirEntry) Type() fs.FileMode          { return e.f.perm.Type() }
func (e *overlayDirEntry) Info() (fs.FileInfo, error) { return &overlayFileInfo{f: e.f}, nil }

type overlayFileInfo struct {
	f *overlayFile
}

func (i *overlayFileInfo) Name() string       { return i.f.name }
func (i *overlayFileInfo) Size() int64        { return int64(len(i.f.data)) }
func (i *overlayFileInfo) Mode() os.FileMode  { return i.f.perm }
func (i *overlayFileInfo) ModTime() time.Time { return i.f.modTime }
func (i *overlayFileInfo) IsDir() bool        { return i.f.isDir }
func (i *overlayFileInfo) Sys() any           { return nil }
