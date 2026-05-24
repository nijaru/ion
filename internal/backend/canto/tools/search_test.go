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
	os.Mkdir(filepath.Join(tmpDir, "src"), 0o755)
	os.WriteFile(filepath.Join(tmpDir, "src", "upper.go"), []byte("Needle\ncontext line"), 0o644)
	os.WriteFile(filepath.Join(tmpDir, "src", "another.go"), []byte("package another"), 0o644)
	os.WriteFile(filepath.Join(tmpDir, "src", "skip.txt"), []byte("Needle"), 0o644)
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
			t.Errorf(
				"expected absolute workspace grep path to render relative results, got %q",
				res,
			)
		}

		outsideDir := t.TempDir()
		if err := os.WriteFile(filepath.Join(outsideDir, "outside.txt"), []byte("search outside"), 0o644); err != nil {
			t.Fatal(err)
		}
		outsideArgs := `{"pattern":"search","path":"` + filepath.ToSlash(outsideDir) + `"}`
		res, err = g.Execute(context.Background(), outsideArgs)
		if err != nil {
			t.Fatalf("grep with absolute outside path failed: %v", err)
		}
		if !strings.Contains(res, "outside.txt") {
			t.Fatalf("expected outside.txt in absolute outside grep results, got %q", res)
		}
		res, err = g.Execute(context.Background(), `{"pattern":"-needle"}`)
		if err != nil {
			t.Fatalf("grep pattern starting with dash failed: %v", err)
		}
		if !strings.Contains(res, "dash.txt") {
			t.Fatalf("expected dash.txt in dash-pattern results, got %q", res)
		}

		res, err = g.Execute(
			context.Background(),
			`{"pattern":"needle","path":"src","glob":"*.go","ignoreCase":true,"literal":true,"context":1,"limit":1}`,
		)
		if err != nil {
			t.Fatalf("grep with pi-like options failed: %v", err)
		}
		if !strings.Contains(res, "src/upper.go") {
			t.Fatalf("expected src/upper.go in filtered grep results, got %q", res)
		}
		if strings.Contains(res, "src/skip.txt") {
			t.Fatalf("glob filter should exclude src/skip.txt, got %q", res)
		}
		if !strings.Contains(res, "matches limit reached") {
			t.Fatalf("expected grep limit notice, got %q", res)
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

	t.Run("Find", func(t *testing.T) {
		find := &Find{SearchTool: *NewSearchTool(tmpDir)}
		if err := os.Symlink(filepath.Join(t.TempDir(), "missing"), filepath.Join(tmpDir, "broken-link")); err != nil {
			t.Logf("symlink unavailable: %v", err)
		}

		args := `{"pattern": "**/*.go"}`
		res, err := find.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("find failed: %v", err)
		}

		if strings.Contains(res, "ignored/") {
			t.Fatalf("expected gitignored paths to be skipped, got %q", res)
		}
		if strings.Contains(res, ".git/") {
			t.Fatalf("expected .git paths to be skipped, got %q", res)
		}
		if strings.TrimSpace(res) != ".hidden.go\nmatch1.go\nsrc/another.go\nsrc/upper.go" {
			t.Errorf("expected hidden and normal go files, got %q", res)
		}

		res, err = find.Execute(context.Background(), `{"pattern":"*.go","path":"src","limit":1}`)
		if err != nil {
			t.Fatalf("find with search path and limit failed: %v", err)
		}
		if !strings.Contains(res, ".go") {
			t.Fatalf("expected path-relative find result, got %q", res)
		}
		if strings.Contains(res, "src/") {
			t.Fatalf("expected find output relative to search path, got %q", res)
		}
		if !strings.Contains(res, "results limit reached") {
			t.Fatalf("expected find limit notice, got %q", res)
		}

		absArgs := `{"pattern":"` + filepath.ToSlash(filepath.Join(tmpDir, "match*")) + `"}`
		res, err = find.Execute(context.Background(), absArgs)
		if err != nil {
			t.Fatalf("find with absolute workspace pattern failed: %v", err)
		}
		if strings.TrimSpace(res) != "match1.go\nmatch2.txt" {
			t.Fatalf("absolute workspace find = %q, want relative matches", res)
		}

		if _, err := find.Execute(context.Background(), `{"pattern":"../*.go"}`); err == nil {
			t.Fatal("expected find pattern outside workspace to fail")
		}

		outsidePattern := filepath.ToSlash(filepath.Join(t.TempDir(), "*.go"))
		if _, err := find.Execute(context.Background(), `{"pattern":"`+outsidePattern+`"}`); err == nil {
			t.Fatal("expected absolute find pattern outside workspace to fail")
		}

		if _, err := find.Execute(context.Background(), `{"pattern":" "}`); err == nil {
			t.Fatal("expected empty find pattern to fail")
		}
	})
}
