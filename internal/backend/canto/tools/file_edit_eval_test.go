package tools

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestEditSurfaceEvalSplitTools(t *testing.T) {
	tmpDir := t.TempDir()

	t.Run("write owns whole-file create and overwrite", func(t *testing.T) {
		w := &Write{FileTool: *newTestFileTool(t, tmpDir)}

		executeToolJSON(t, w, context.Background(), map[string]any{
			"file_path": "created.txt",
			"content":   "alpha\n",
		})
		executeToolJSON(t, w, context.Background(), map[string]any{
			"file_path": "created.txt",
			"content":   "omega\n",
		})

		assertFileContent(t, tmpDir, "created.txt", "omega\n")
	})

	t.Run("edit owns targeted replacement with CRLF and BOM preservation", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		path := "windows-eval.txt"
		writeFile(t, tmpDir, path, "\ufeffalpha\r\nbeta\r\n")

		executeToolJSON(t, e, context.Background(), map[string]any{
			"file_path":  path,
			"old_string": "alpha\nbeta",
			"new_string": "one\ntwo",
		})

		assertFileContent(t, tmpDir, path, "\ufeffone\r\ntwo\r\n")
	})

	t.Run("multi_edit validates all edits before writing", func(t *testing.T) {
		m := &MultiEdit{FileTool: *newTestFileTool(t, tmpDir)}
		writeFile(t, tmpDir, "atomic-a.txt", "alpha\n")
		writeFile(t, tmpDir, "atomic-b.txt", "beta\n")

		_, err := executeToolJSON(t, m, context.Background(), map[string]any{
			"edits": []map[string]any{
				{
					"file_path":  "atomic-a.txt",
					"old_string": "alpha",
					"new_string": "one",
				},
				{
					"file_path":  "atomic-b.txt",
					"old_string": "missing",
					"new_string": "two",
				},
			},
		})
		if err == nil {
			t.Fatal("expected validation failure")
		}

		assertFileContent(t, tmpDir, "atomic-a.txt", "alpha\n")
		assertFileContent(t, tmpDir, "atomic-b.txt", "beta\n")
	})

	t.Run("multi_edit handles multi-file successful edits", func(t *testing.T) {
		m := &MultiEdit{FileTool: *newTestFileTool(t, tmpDir)}
		writeFile(t, tmpDir, "multi-a.txt", "alpha\n")
		writeFile(t, tmpDir, "multi-b.txt", "beta\n")

		result, err := executeToolJSON(t, m, context.Background(), map[string]any{
			"edits": []map[string]any{
				{
					"file_path":  "multi-a.txt",
					"old_string": "alpha",
					"new_string": "one",
				},
				{
					"file_path":  "multi-b.txt",
					"old_string": "beta",
					"new_string": "two",
				},
			},
		})
		if err != nil {
			t.Fatalf("multi_edit failed: %v", err)
		}
		if !strings.Contains(result, "--- a/multi-a.txt") ||
			!strings.Contains(result, "--- a/multi-b.txt") {
			t.Fatalf("multi_edit result missing per-file diffs: %q", result)
		}

		assertFileContent(t, tmpDir, "multi-a.txt", "one\n")
		assertFileContent(t, tmpDir, "multi-b.txt", "two\n")
	})

	t.Run("edit reports duplicate and expected-count failures with lines", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		path := "counts.txt"
		writeFile(t, tmpDir, path, "same\nsame\n")

		_, err := executeToolJSON(t, e, context.Background(), map[string]any{
			"file_path":  path,
			"old_string": "same",
			"new_string": "next",
		})
		if err == nil || !strings.Contains(err.Error(), "line(s) 1, 2") {
			t.Fatalf("duplicate-match error = %v, want line numbers", err)
		}

		_, err = executeToolJSON(t, e, context.Background(), map[string]any{
			"file_path":             path,
			"old_string":            "same",
			"new_string":            "next",
			"replace_all":           true,
			"expected_replacements": 1,
		})
		if err == nil ||
			!strings.Contains(err.Error(), "expected 1 replacement(s)") ||
			!strings.Contains(err.Error(), "line(s) 1, 2") {
			t.Fatalf("expected-count error = %v, want count and line numbers", err)
		}
	})

	t.Run("canceled edit leaves file unchanged", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		path := "cancel.txt"
		writeFile(t, tmpDir, path, "before\n")
		ctx, cancel := context.WithCancel(context.Background())
		cancel()

		_, err := executeToolJSON(t, e, ctx, map[string]any{
			"file_path":  path,
			"old_string": "before",
			"new_string": "after",
		})
		if err == nil {
			t.Fatal("expected canceled context to fail")
		}

		assertFileContent(t, tmpDir, path, "before\n")
	})
}

type jsonTool interface {
	Execute(context.Context, string) (string, error)
}

func executeToolJSON(
	t *testing.T,
	tool jsonTool,
	ctx context.Context,
	args map[string]any,
) (string, error) {
	t.Helper()
	data, err := json.Marshal(args)
	if err != nil {
		t.Fatal(err)
	}
	return tool.Execute(ctx, string(data))
}

func writeFile(t *testing.T, root, name, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(root, name), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

func assertFileContent(t *testing.T, root, name, want string) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(root, name))
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != want {
		t.Fatalf("%s = %q, want %q", name, data, want)
	}
}
