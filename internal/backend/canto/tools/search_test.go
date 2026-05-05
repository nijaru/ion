package tools

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestSearchTools(t *testing.T) {
	tmpDir := t.TempDir()

	// Setup test files
	os.WriteFile(
		filepath.Join(tmpDir, "match1.go"),
		[]byte("package tools\n\nfunc Search() {}"),
		0o644,
	)
	os.WriteFile(filepath.Join(tmpDir, "match2.txt"), []byte("useful search results here"), 0o644)
	os.WriteFile(filepath.Join(tmpDir, "nomatch.log"), []byte("nothing here"), 0o644)
	os.WriteFile(filepath.Join(tmpDir, "dash.txt"), []byte("-needle"), 0o644)
	os.WriteFile(filepath.Join(tmpDir, ".hidden.go"), []byte("package hidden"), 0o644)
	os.Mkdir(filepath.Join(tmpDir, "ignored"), 0o755)
	os.WriteFile(filepath.Join(tmpDir, "ignored", "ignored.go"), []byte("package ignored"), 0o644)
	os.WriteFile(filepath.Join(tmpDir, ".gitignore"), []byte("ignored/**\n"), 0o644)
	os.Mkdir(filepath.Join(tmpDir, ".git"), 0o755)
	os.WriteFile(filepath.Join(tmpDir, ".git", "hidden.txt"), []byte("search me not"), 0o644)
	os.WriteFile(filepath.Join(tmpDir, ".git", "internal.go"), []byte("package git"), 0o644)

	t.Run("Grep", func(t *testing.T) {
		g := &Grep{SearchTool: *NewSearchTool(tmpDir)}

		// Search for pattern in multiple files
		args := `{"pattern": "search"}`
		res, err := g.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("grep failed: %v", err)
		}

		if !strings.Contains(res, "match2.txt") {
			t.Errorf("expected match2.txt in results, got %q", res)
		}
		if strings.Contains(res, tmpDir) {
			t.Errorf("expected relative grep paths, got %q", res)
		}
		if strings.Contains(res, ".git") {
			t.Error("expected .git directory to be ignored")
		}

		absArgs := `{"pattern":"search","path":"` + filepath.ToSlash(tmpDir) + `"}`
		res, err = g.Execute(context.Background(), absArgs)
		if err != nil {
			t.Fatalf("grep with absolute workspace path failed: %v", err)
		}
		if strings.Contains(res, tmpDir) {
			t.Errorf("expected absolute workspace grep path to render relative results, got %q", res)
		}

		if _, err := g.Execute(context.Background(), `{"pattern":"search","path":".."}`); err == nil {
			t.Fatal("expected grep path outside workspace to fail")
		}

		outsideDir := t.TempDir()
		if err := os.WriteFile(filepath.Join(outsideDir, "outside.txt"), []byte("search outside"), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(outsideDir, filepath.Join(tmpDir, "outside-link")); err != nil {
			t.Skipf("symlink unavailable: %v", err)
		}
		if _, err := g.Execute(context.Background(), `{"pattern":"search","path":"outside-link"}`); err == nil {
			t.Fatal("expected grep symlink path outside workspace to fail")
		}
		if err := os.Remove(filepath.Join(tmpDir, "outside-link")); err != nil {
			t.Fatal(err)
		}

		res, err = g.Execute(context.Background(), `{"pattern":"-needle"}`)
		if err != nil {
			t.Fatalf("grep pattern starting with dash failed: %v", err)
		}
		if !strings.Contains(res, "dash.txt") {
			t.Fatalf("expected dash.txt in dash-pattern results, got %q", res)
		}

		res, err = g.Execute(context.Background(), `{"pattern":"definitely-not-present"}`)
		if err != nil {
			t.Fatalf("grep no-match should not be a tool error: %v", err)
		}
		if strings.TrimSpace(res) != "No matches found." {
			t.Fatalf("grep no-match = %q", res)
		}

		if _, err := g.Execute(context.Background(), `{"pattern":" "}`); err == nil {
			t.Fatal("expected empty grep pattern to fail")
		}
	})

	t.Run("Glob", func(t *testing.T) {
		gl := &Glob{SearchTool: *NewSearchTool(tmpDir)}
		if err := os.Symlink(filepath.Join(t.TempDir(), "missing"), filepath.Join(tmpDir, "broken-link")); err != nil {
			t.Logf("symlink unavailable: %v", err)
		}

		args := `{"pattern": "**/*.go"}`
		res, err := gl.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("glob failed: %v", err)
		}

		if strings.Contains(res, "ignored/") {
			t.Fatalf("expected gitignored paths to be skipped, got %q", res)
		}
		if strings.Contains(res, ".git/") {
			t.Fatalf("expected .git paths to be skipped, got %q", res)
		}
		if strings.TrimSpace(res) != ".hidden.go\nmatch1.go" {
			t.Errorf("expected hidden and normal go files, got %q", res)
		}

		if _, err := gl.Execute(context.Background(), `{"pattern":"../*.go"}`); err == nil {
			t.Fatal("expected glob pattern outside workspace to fail")
		}

		if _, err := gl.Execute(context.Background(), `{"pattern":" "}`); err == nil {
			t.Fatal("expected empty glob pattern to fail")
		}
	})
}
