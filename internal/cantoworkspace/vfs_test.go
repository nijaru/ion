package workspace

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"
)

type mockFS struct {
	WorkspaceFS
	data map[string]string
	dirs map[string]struct{}
}

func (m *mockFS) ReadFile(name string) ([]byte, error) {
	name = path.Clean(name)
	if d, ok := m.data[name]; ok {
		return []byte(d), nil
	}
	return nil, os.ErrNotExist
}

func (m *mockFS) WriteFile(name string, data []byte, perm os.FileMode) error {
	name = path.Clean(name)
	if m.dirs == nil {
		m.dirs = make(map[string]struct{})
	}
	for dir := path.Dir(name); dir != "." && dir != "/"; dir = path.Dir(dir) {
		m.dirs[dir] = struct{}{}
	}
	m.data[name] = string(data)
	return nil
}

func (m *mockFS) MkdirAll(name string, perm os.FileMode) error {
	if m.dirs == nil {
		m.dirs = make(map[string]struct{})
	}
	name = path.Clean(name)
	for dir := name; dir != "." && dir != "/"; dir = path.Dir(dir) {
		m.dirs[dir] = struct{}{}
	}
	return nil
}

func (m *mockFS) Remove(name string) error {
	name = path.Clean(name)
	delete(m.data, name)
	delete(m.dirs, name)
	return nil
}

func (m *mockFS) ReadDir(name string) ([]fs.DirEntry, error) {
	name = path.Clean(name)
	entries := map[string]fs.DirEntry{}
	for file := range m.data {
		if path.Dir(file) == name {
			entries[path.Base(file)] = testDirEntry{name: path.Base(file)}
		}
	}
	for dir := range m.dirs {
		if path.Dir(dir) == name {
			entries[path.Base(dir)] = testDirEntry{name: path.Base(dir), isDir: true}
		}
	}
	res := make([]fs.DirEntry, 0, len(entries))
	for _, entry := range entries {
		res = append(res, entry)
	}
	slices.SortFunc(res, func(a, b fs.DirEntry) int {
		return strings.Compare(a.Name(), b.Name())
	})
	if len(res) == 0 {
		return nil, os.ErrNotExist
	}
	return res, nil
}

func (m *mockFS) Stat(name string) (fs.FileInfo, error) {
	name = path.Clean(name)
	if content, ok := m.data[name]; ok {
		return testFileInfo{name: path.Base(name), size: int64(len(content))}, nil
	}
	if _, ok := m.dirs[name]; ok {
		return testFileInfo{name: path.Base(name), mode: os.ModeDir, isDir: true}, nil
	}
	return nil, os.ErrNotExist
}

func (m *mockFS) Glob(_ context.Context, pattern string) ([]string, error) {
	var matches []string
	for file := range m.data {
		ok, err := path.Match(pattern, file)
		if err != nil {
			return nil, err
		}
		if ok {
			matches = append(matches, file)
		}
	}
	slices.Sort(matches)
	return matches, nil
}

func (m *mockFS) Path() string { return "mock://" }
func (m *mockFS) Close() error { return nil }

type testDirEntry struct {
	name  string
	isDir bool
}

func (e testDirEntry) Name() string { return e.name }
func (e testDirEntry) IsDir() bool  { return e.isDir }
func (e testDirEntry) Type() fs.FileMode {
	if e.isDir {
		return os.ModeDir
	}
	return 0
}

func (e testDirEntry) Info() (fs.FileInfo, error) {
	return testFileInfo{name: e.name, mode: e.Type(), isDir: e.isDir}, nil
}

type testFileInfo struct {
	name  string
	size  int64
	mode  os.FileMode
	isDir bool
}

func (i testFileInfo) Name() string       { return i.name }
func (i testFileInfo) Size() int64        { return i.size }
func (i testFileInfo) Mode() os.FileMode  { return i.mode }
func (i testFileInfo) ModTime() time.Time { return time.Time{} }
func (i testFileInfo) IsDir() bool        { return i.isDir }
func (i testFileInfo) Sys() any           { return nil }

func TestOverlayFS(t *testing.T) {
	base := &mockFS{data: map[string]string{"base.txt": "base content"}}
	overlay := NewOverlayFS(base)

	// Read from base
	data, err := overlay.ReadFile("base.txt")
	if err != nil {
		t.Errorf("ReadFile base.txt: %v", err)
	}
	if string(data) != "base content" {
		t.Errorf("wrong content: %q", string(data))
	}

	// Write speculative
	if err := overlay.WriteFile("spec.txt", []byte("spec content"), 0o644); err != nil {
		t.Fatalf("WriteFile spec.txt: %v", err)
	}

	// Read speculative
	data, err = overlay.ReadFile("spec.txt")
	if err != nil {
		t.Errorf("ReadFile spec.txt: %v", err)
	}
	if string(data) != "spec content" {
		t.Errorf("wrong spec content: %q", string(data))
	}

	// Base remains untouched
	if _, ok := base.data["spec.txt"]; ok {
		t.Error("base should not have spec.txt yet")
	}

	// Test Snapshot
	snap := overlay.Snapshot()

	// Modify more
	if err := overlay.WriteFile("spec2.txt", []byte("spec2 content"), 0o644); err != nil {
		t.Fatalf("WriteFile spec2.txt: %v", err)
	}

	// Restore Snapshot
	overlay.RestoreSnapshot(snap)

	// spec2.txt should be gone
	if _, err := overlay.ReadFile("spec2.txt"); !os.IsNotExist(err) {
		t.Errorf("spec2.txt should not exist after restore, got error: %v", err)
	}
	// spec.txt should still exist
	if _, err := overlay.ReadFile("spec.txt"); err != nil {
		t.Errorf("spec.txt should still exist after restore: %v", err)
	}

	// Test Commit
	if err := overlay.Commit(t.Context()); err != nil {
		t.Fatalf("Commit: %v", err)
	}

	// Base should now have spec.txt
	if base.data["spec.txt"] != "spec content" {
		t.Errorf("base.txt missing from base after commit, got %q", base.data["spec.txt"])
	}

	// Speculative should be empty
	if len(overlay.speculative) != 0 {
		t.Error("speculative should be empty after commit")
	}

	// Test Delete Speculative
	if err := overlay.Remove("base.txt"); err != nil {
		t.Fatalf("Remove: %v", err)
	}
	if _, err := overlay.ReadFile("base.txt"); !os.IsNotExist(err) {
		t.Errorf("base.txt should be deleted from overlay, got error: %v", err)
	}
	// But still in base until commit
	if _, ok := base.data["base.txt"]; !ok {
		t.Error("base.txt should still be in base until commit")
	}

	if err := overlay.Commit(t.Context()); err != nil {
		t.Fatalf("Commit delete: %v", err)
	}
	if _, ok := base.data["base.txt"]; ok {
		t.Error("base.txt should be removed from base after commit")
	}
}

func TestOverlayFS_ReadDirStatAndDiscard(t *testing.T) {
	base := &mockFS{
		data: map[string]string{
			"base.txt":        "base",
			"nested/base.txt": "nested base",
		},
		dirs: map[string]struct{}{"nested": {}},
	}
	overlay := NewOverlayFS(base)

	if err := overlay.WriteFile("nested/spec.txt", []byte("spec"), 0o644); err != nil {
		t.Fatalf("WriteFile nested/spec.txt: %v", err)
	}
	if err := overlay.MkdirAll("empty/child", 0o755); err != nil {
		t.Fatalf("MkdirAll empty/child: %v", err)
	}
	if err := overlay.Remove("base.txt"); err != nil {
		t.Fatalf("Remove base.txt: %v", err)
	}

	entries, err := overlay.ReadDir(".")
	if err != nil {
		t.Fatalf("ReadDir .: %v", err)
	}
	names := entryNames(entries)
	if !slices.Equal(names, []string{"empty", "nested"}) {
		t.Fatalf("root entries = %#v, want empty,nested", names)
	}

	entries, err = overlay.ReadDir("nested")
	if err != nil {
		t.Fatalf("ReadDir nested: %v", err)
	}
	names = entryNames(entries)
	if !slices.Equal(names, []string{"base.txt", "spec.txt"}) {
		t.Fatalf("nested entries = %#v, want base.txt,spec.txt", names)
	}

	info, err := overlay.Stat("nested/spec.txt")
	if err != nil {
		t.Fatalf("Stat nested/spec.txt: %v", err)
	}
	if info.Name() != "spec.txt" || info.Size() != 4 || info.IsDir() {
		t.Fatalf("unexpected spec file info: %#v", info)
	}
	if _, err := overlay.Stat("base.txt"); !os.IsNotExist(err) {
		t.Fatalf("deleted base.txt stat error = %v, want not exist", err)
	}

	overlay.Discard()
	if got, err := overlay.ReadFile("base.txt"); err != nil || string(got) != "base" {
		t.Fatalf("discard should restore base.txt, got %q err %v", string(got), err)
	}
	if _, err := overlay.ReadFile("nested/spec.txt"); !os.IsNotExist(err) {
		t.Fatalf("discard should remove spec file, got %v", err)
	}
}

func TestOverlayFSRejectsEscapingSpeculativePaths(t *testing.T) {
	base := &mockFS{data: map[string]string{}}
	overlay := NewOverlayFS(base)

	if err := overlay.WriteFile("../escape.txt", []byte("escape"), 0o644); !errors.Is(
		err,
		ErrPathTraversal,
	) {
		t.Fatalf("WriteFile traversal err = %v, want ErrPathTraversal", err)
	}
	if _, err := overlay.ReadFile("../escape.txt"); !errors.Is(err, ErrPathTraversal) {
		t.Fatalf("ReadFile traversal err = %v, want ErrPathTraversal", err)
	}
	if err := overlay.WriteFile("/tmp/escape.txt", []byte("escape"), 0o644); !errors.Is(
		err,
		ErrAbsolutePath,
	) {
		t.Fatalf("WriteFile absolute err = %v, want ErrAbsolutePath", err)
	}
	if _, err := overlay.ReadFile(".."); !errors.Is(err, ErrPathTraversal) {
		t.Fatalf("ReadFile parent err = %v, want ErrPathTraversal", err)
	}
}

func TestOverlayFSGlobMergesSpeculativeAndDeletedFiles(t *testing.T) {
	base := &mockFS{
		data: map[string]string{
			"base.txt":        "base",
			"deleted.txt":     "deleted",
			"nested/base.txt": "nested base",
		},
		dirs: map[string]struct{}{"nested": {}},
	}
	overlay := NewOverlayFS(base)
	if err := overlay.WriteFile("spec.txt", []byte("spec"), 0o644); err != nil {
		t.Fatalf("WriteFile spec.txt: %v", err)
	}
	if err := overlay.WriteFile("nested/spec.txt", []byte("spec"), 0o644); err != nil {
		t.Fatalf("WriteFile nested/spec.txt: %v", err)
	}
	if err := overlay.Remove("deleted.txt"); err != nil {
		t.Fatalf("Remove deleted.txt: %v", err)
	}

	matches, err := overlay.Glob(t.Context(), "*.txt")
	if err != nil {
		t.Fatalf("Glob root txt: %v", err)
	}
	if !slices.Equal(matches, []string{"base.txt", "spec.txt"}) {
		t.Fatalf("root glob = %#v, want base/spec", matches)
	}

	matches, err = overlay.Glob(t.Context(), "nested/*.txt")
	if err != nil {
		t.Fatalf("Glob nested txt: %v", err)
	}
	if !slices.Equal(matches, []string{"nested/base.txt", "nested/spec.txt"}) {
		t.Fatalf("nested glob = %#v, want base/spec", matches)
	}
}

func TestOverlayFSStandardFSReflectsSpeculativeState(t *testing.T) {
	base := &mockFS{
		data: map[string]string{
			"base.txt":    "base",
			"deleted.txt": "deleted",
		},
		dirs: map[string]struct{}{},
	}
	overlay := NewOverlayFS(base)
	if err := overlay.WriteFile("spec.txt", []byte("spec"), 0o644); err != nil {
		t.Fatalf("WriteFile spec.txt: %v", err)
	}
	if err := overlay.Remove("deleted.txt"); err != nil {
		t.Fatalf("Remove deleted.txt: %v", err)
	}

	fsys := overlay.FS()
	data, err := fs.ReadFile(fsys, "spec.txt")
	if err != nil {
		t.Fatalf("fs.ReadFile spec.txt: %v", err)
	}
	if string(data) != "spec" {
		t.Fatalf("spec file data = %q, want spec", data)
	}
	if _, err := fs.ReadFile(fsys, "deleted.txt"); !os.IsNotExist(err) {
		t.Fatalf("fs.ReadFile deleted.txt error = %v, want not exist", err)
	}

	entries, err := fs.ReadDir(fsys, ".")
	if err != nil {
		t.Fatalf("fs.ReadDir .: %v", err)
	}
	names := entryNames(entries)
	if !slices.Equal(names, []string{"base.txt", "spec.txt"}) {
		t.Fatalf("standard fs root entries = %#v, want base/spec", names)
	}
}

func TestOverlayFSConcurrentReadDirAndMutations(t *testing.T) {
	base := &mockFS{
		data: map[string]string{"base.txt": "base"},
		dirs: map[string]struct{}{},
	}
	overlay := NewOverlayFS(base)

	var wg sync.WaitGroup
	errs := make(chan error, 64)
	for i := range 32 {
		wg.Go(func() {
			name := fmt.Sprintf("spec-%02d.txt", i)
			errs <- overlay.WriteFile(name, []byte("spec"), 0o644)
		})
		wg.Go(func() {
			_, err := overlay.ReadDir(".")
			errs <- err
		})
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatalf("overlay operation: %v", err)
		}
	}
}

func TestMultiFS(t *testing.T) {
	base := &mockFS{data: map[string]string{"base.txt": "base content"}}
	mount := &mockFS{data: map[string]string{"mount.txt": "mount content"}}

	multi := NewMultiFS(base)
	multi.Mount("memory", mount)

	// Read from base
	data, err := multi.ReadFile("base.txt")
	if err != nil {
		t.Errorf("ReadFile base.txt: %v", err)
	}
	if string(data) != "base content" {
		t.Errorf("wrong base content: %q", string(data))
	}

	// Read from mount
	data, err = multi.ReadFile("memory/mount.txt")
	if err != nil {
		t.Errorf("ReadFile memory/mount.txt: %v", err)
	}
	if string(data) != "mount content" {
		t.Errorf("wrong mount content: %q", string(data))
	}

	// Read non-existent from mount
	_, err = multi.ReadFile("memory/missing.txt")
	if !os.IsNotExist(err) {
		t.Errorf("expected NotExist for memory/missing.txt, got %v", err)
	}
}

func TestMultiFSRoutesMountedOperations(t *testing.T) {
	base := &mockFS{
		data: map[string]string{"base.txt": "base content"},
		dirs: map[string]struct{}{},
	}
	mount := &mockFS{
		data: map[string]string{"mount.txt": "mount content"},
		dirs: map[string]struct{}{},
	}

	multi := NewMultiFS(base)
	multi.Mount("memory", mount)

	if err := multi.WriteFile("memory/new.txt", []byte("mounted write"), 0o644); err != nil {
		t.Fatalf("mounted WriteFile: %v", err)
	}
	if _, ok := base.data["memory/new.txt"]; ok {
		t.Fatal("mounted write should not hit base filesystem")
	}
	if got := mount.data["new.txt"]; got != "mounted write" {
		t.Fatalf("mounted data = %q, want mounted write", got)
	}

	if err := multi.MkdirAll("memory/docs", 0o755); err != nil {
		t.Fatalf("mounted MkdirAll: %v", err)
	}
	if _, ok := mount.dirs["docs"]; !ok {
		t.Fatal("mounted directory was not created")
	}

	info, err := multi.Stat("memory/new.txt")
	if err != nil {
		t.Fatalf("mounted Stat: %v", err)
	}
	if info.Name() != "new.txt" || info.Size() != int64(len("mounted write")) {
		t.Fatalf("unexpected mounted stat: %#v", info)
	}

	entries, err := multi.ReadDir(".")
	if err != nil {
		t.Fatalf("ReadDir root: %v", err)
	}
	names := entryNames(entries)
	if !slices.Equal(names, []string{"base.txt", "memory"}) {
		t.Fatalf("root entries = %#v, want base.txt,memory", names)
	}
	info, err = entries[1].Info()
	if err != nil {
		t.Fatalf("virtual mount Info: %v", err)
	}
	if info == nil || !info.IsDir() || info.Name() != "memory" {
		t.Fatalf("virtual mount info = %#v, want directory named memory", info)
	}

	if err := multi.Remove("memory/new.txt"); err != nil {
		t.Fatalf("mounted Remove: %v", err)
	}
	if _, ok := mount.data["new.txt"]; ok {
		t.Fatal("mounted remove should delete from mount")
	}

	if err := multi.WriteFile("base2.txt", []byte("base write"), 0o644); err != nil {
		t.Fatalf("base WriteFile: %v", err)
	}
	if got := base.data["base2.txt"]; got != "base write" {
		t.Fatalf("base data = %q, want base write", got)
	}
}

func TestMultiFSGlobIncludesMountedFilesystem(t *testing.T) {
	base := &mockFS{
		data: map[string]string{"base.txt": "base content"},
		dirs: map[string]struct{}{},
	}
	mount := &mockFS{
		data: map[string]string{"mount.txt": "mount content"},
		dirs: map[string]struct{}{},
	}

	multi := NewMultiFS(base)
	multi.Mount("memory", mount)

	matches, err := multi.Glob(t.Context(), "*.txt")
	if err != nil {
		t.Fatalf("Glob base: %v", err)
	}
	if !slices.Equal(matches, []string{"base.txt"}) {
		t.Fatalf("base glob = %#v, want base.txt", matches)
	}

	matches, err = multi.Glob(t.Context(), "memory/*.txt")
	if err != nil {
		t.Fatalf("Glob mounted: %v", err)
	}
	if !slices.Equal(matches, []string{"memory/mount.txt"}) {
		t.Fatalf("mounted glob = %#v, want memory/mount.txt", matches)
	}

	if _, err := multi.Glob(t.Context(), "../*.txt"); !errors.Is(err, ErrPathTraversal) {
		t.Fatalf("traversal glob error = %v, want ErrPathTraversal", err)
	}
	if _, err := multi.Glob(t.Context(), "/tmp/*.txt"); !errors.Is(err, ErrAbsolutePath) {
		t.Fatalf("absolute glob error = %v, want ErrAbsolutePath", err)
	}
}

func entryNames(entries []fs.DirEntry) []string {
	names := make([]string, len(entries))
	for i, entry := range entries {
		names[i] = entry.Name()
	}
	return names
}
