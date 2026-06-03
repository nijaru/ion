package workspace

import (
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"testing"
)

func TestRootReadWriteListAndGlob(t *testing.T) {
	dir := t.TempDir()

	root, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if err := root.WriteFile("nested/hello.txt", []byte("hi"), 0o644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}
	if err := root.WriteFile("nested/deep/child.txt", []byte("hi"), 0o644); err != nil {
		t.Fatalf("WriteFile deep: %v", err)
	}

	data, err := root.ReadFile("nested/hello.txt")
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	if string(data) != "hi" {
		t.Fatalf("ReadFile = %q, want %q", string(data), "hi")
	}

	entries, err := root.ReadDir("nested")
	if err != nil {
		t.Fatalf("ReadDir: %v", err)
	}
	if len(entries) != 2 ||
		entries[0].Name() != "deep" ||
		entries[1].Name() != "hello.txt" {
		t.Fatalf("unexpected entries: %#v", entries)
	}

	matches, err := root.Glob(t.Context(), "nested/*.txt")
	if err != nil {
		t.Fatalf("Glob: %v", err)
	}
	slices.Sort(matches)
	if !slices.Equal(matches, []string{"nested/hello.txt"}) {
		t.Fatalf("Glob = %#v, want %#v", matches, []string{"nested/hello.txt"})
	}

	matches, err = root.Glob(t.Context(), "nested/**/*.txt")
	if err != nil {
		t.Fatalf("Glob recursive: %v", err)
	}
	slices.Sort(matches)
	if !slices.Equal(matches, []string{"nested/deep/child.txt", "nested/hello.txt"}) {
		t.Fatalf(
			"recursive Glob = %#v, want %#v",
			matches,
			[]string{"nested/deep/child.txt", "nested/hello.txt"},
		)
	}
}

func TestRootStat(t *testing.T) {
	dir := t.TempDir()

	root, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if err := root.WriteFile("nested/hello.txt", []byte("hi"), 0o644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	info, err := root.Stat("nested/hello.txt")
	if err != nil {
		t.Fatalf("Stat: %v", err)
	}
	if info.IsDir() {
		t.Fatal("expected file stat, got directory")
	}
	if info.Name() != "hello.txt" {
		t.Fatalf("Stat.Name = %q, want hello.txt", info.Name())
	}
}

func TestRootMkdirAllAndRemove(t *testing.T) {
	dir := t.TempDir()

	root, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if err := root.MkdirAll("nested/child", 0o755); err != nil {
		t.Fatalf("MkdirAll: %v", err)
	}
	if info, err := root.Stat("nested/child"); err != nil || !info.IsDir() {
		t.Fatalf("Stat nested/child = %#v, %v; want directory", info, err)
	}

	if err := root.WriteFile("nested/child/hello.txt", []byte("hi"), 0o644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}
	if err := root.Remove("nested/child/hello.txt"); err != nil {
		t.Fatalf("Remove file: %v", err)
	}
	if _, err := root.Stat("nested/child/hello.txt"); !os.IsNotExist(err) {
		t.Fatalf("removed file stat error = %v, want not exist", err)
	}
}

func TestRootRejectsSymlinkEscapes(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink permissions are environment-dependent on Windows")
	}

	dir := t.TempDir()
	outside := filepath.Join(t.TempDir(), "secret.txt")
	if err := os.WriteFile(outside, []byte("secret"), 0o644); err != nil {
		t.Fatalf("WriteFile outside: %v", err)
	}
	if err := os.Symlink(outside, filepath.Join(dir, "escape.txt")); err != nil {
		t.Fatalf("Symlink: %v", err)
	}

	root, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if _, err := root.ReadFile("escape.txt"); err == nil {
		t.Fatal("expected symlink escape read to fail")
	}
}

func TestRootRejectsAbsoluteAndTraversalPaths(t *testing.T) {
	dir := t.TempDir()

	root, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if err := root.WriteFile("../escape.txt", []byte("nope"), 0o644); err == nil {
		t.Fatal("expected traversal write to fail")
	}
	if _, err := root.ReadFile(filepath.Join(dir, "abs.txt")); err == nil {
		t.Fatal("expected absolute read to fail")
	}
	if _, err := root.Glob(t.Context(), filepath.Join(dir, "*.txt")); err == nil {
		t.Fatal("expected absolute glob to fail")
	}
	if _, err := root.Glob(t.Context(), "../*.txt"); err == nil {
		t.Fatal("expected traversal glob to fail")
	}
}

func TestRootReadDirAllowsWorkspaceRoot(t *testing.T) {
	dir := t.TempDir()

	root, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if err := root.WriteFile("hello.txt", []byte("hi"), 0o644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}
	entries, err := root.ReadDir(".")
	if err != nil {
		t.Fatalf("ReadDir(.): %v", err)
	}
	if len(entries) != 1 || entries[0].Name() != "hello.txt" {
		t.Fatalf("unexpected root entries: %#v", entries)
	}
}
