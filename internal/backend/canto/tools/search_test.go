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
	os.WriteFile(filepath.Join(tmpDir, "match1.go"), []byte("package tools\n\nfunc Search() {}"), 0644)
	os.WriteFile(filepath.Join(tmpDir, "match2.txt"), []byte("useful search results here"), 0644)
	os.WriteFile(filepath.Join(tmpDir, "nomatch.log"), []byte("nothing here"), 0644)
	os.Mkdir(filepath.Join(tmpDir, ".git"), 0755)
	os.WriteFile(filepath.Join(tmpDir, ".git", "hidden.txt"), []byte("search me not"), 0644)

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
		if strings.Contains(res, ".git") {
			t.Error("expected .git directory to be ignored")
		}
	})

	t.Run("Glob", func(t *testing.T) {
		gl := &Glob{SearchTool: *NewSearchTool(tmpDir)}
		
		args := `{"pattern": "**/*.go"}`
		res, err := gl.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("glob failed: %v", err)
		}
		
		if strings.TrimSpace(res) != "match1.go" {
			t.Errorf("expected match1.go, got %q", res)
		}
	})
}
