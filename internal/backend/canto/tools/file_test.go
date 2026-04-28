package tools

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

func newTestFileTool(t *testing.T, cwd string) *FileTool {
	t.Helper()
	return &FileTool{
		cwd:        cwd,
		checkpoint: ionworkspace.NewCheckpointStore(filepath.Join(t.TempDir(), "checkpoints")),
	}
}

func TestFileTools(t *testing.T) {
	tmpDir := t.TempDir()

	t.Run("Write and Read", func(t *testing.T) {
		w := &Write{FileTool: *newTestFileTool(t, tmpDir)}
		r := &Read{FileTool: *newTestFileTool(t, tmpDir)}

		filePath := "test.txt"
		content := "line 1\nline 2\nline 3"

		// Write
		writeArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"content":   content,
		})
		writeResult, err := w.Execute(context.Background(), string(writeArgs))
		if err != nil {
			t.Fatalf("write failed: %v", err)
		}
		if !strings.Contains(writeResult, "Checkpoint: ") {
			t.Fatalf("write result missing checkpoint id: %q", writeResult)
		}

		// Read full
		readArgs, _ := json.Marshal(map[string]any{"file_path": filePath})
		res, err := r.Execute(context.Background(), string(readArgs))
		if err != nil {
			t.Fatalf("read failed: %v", err)
		}
		if res != content {
			t.Errorf("expected %q, got %q", content, res)
		}

		// Read with limit/offset
		limitArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"offset":    1,
			"limit":     1,
		})
		res, err = r.Execute(context.Background(), string(limitArgs))
		if err != nil {
			t.Fatalf("read with limit failed: %v", err)
		}
		if res != "line 2" {
			t.Errorf("expected line 2, got %q", res)
		}

		negativeOffsetArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"offset":    -1,
			"limit":     1,
		})
		if _, err := r.Execute(context.Background(), string(negativeOffsetArgs)); err == nil {
			t.Fatal("expected negative offset to fail")
		}
	})

	t.Run("Edit", func(t *testing.T) {
		e := &Edit{FileTool: *newTestFileTool(t, tmpDir)}
		filePath := "edit-test.txt"
		content := "foo\nbar\nbaz"
		os.WriteFile(filepath.Join(tmpDir, filePath), []byte(content), 0644)

		// Replace unique
		editArgs, _ := json.Marshal(map[string]any{
			"file_path":  filePath,
			"old_string": "bar",
			"new_string": "qux",
		})
		_, err := e.Execute(context.Background(), string(editArgs))
		if err != nil {
			t.Fatalf("edit failed: %v", err)
		}

		newContent, _ := os.ReadFile(filepath.Join(tmpDir, filePath))
		if string(newContent) != "foo\nqux\nbaz" {
			t.Errorf("unexpected content: %q", string(newContent))
		}

		// Fail on non-unique without replace_all
		os.WriteFile(filepath.Join(tmpDir, filePath), []byte("aa\naa"), 0644)
		failArgs, _ := json.Marshal(map[string]any{
			"file_path":  filePath,
			"old_string": "aa",
			"new_string": "bb",
		})
		_, err = e.Execute(context.Background(), string(failArgs))
		if err == nil {
			t.Error("expected error for non-unique match, got nil")
		}

		// Succeed on non-unique with replace_all
		allArgs, _ := json.Marshal(map[string]any{
			"file_path":   filePath,
			"old_string":  "aa",
			"new_string":  "bb",
			"replace_all": true,
		})
		_, err = e.Execute(context.Background(), string(allArgs))
		if err != nil {
			t.Fatalf("edit all failed: %v", err)
		}
		newContent, _ = os.ReadFile(filepath.Join(tmpDir, filePath))
		if string(newContent) != "bb\nbb" {
			t.Errorf("unexpected content: %q", string(newContent))
		}

		emptyOldArgs, _ := json.Marshal(map[string]any{
			"file_path":  filePath,
			"old_string": "",
			"new_string": "x",
		})
		if _, err := e.Execute(context.Background(), string(emptyOldArgs)); err == nil {
			t.Fatal("expected empty old_string to fail")
		}

		noopArgs, _ := json.Marshal(map[string]any{
			"file_path":  filePath,
			"old_string": "bb",
			"new_string": "bb",
		})
		if _, err := e.Execute(context.Background(), string(noopArgs)); err == nil {
			t.Fatal("expected no-op edit to fail")
		}
	})

	t.Run("MultiEdit", func(t *testing.T) {
		m := &MultiEdit{FileTool: *newTestFileTool(t, tmpDir)}

		f1 := "file1.txt"
		f2 := "file2.txt"
		os.WriteFile(filepath.Join(tmpDir, f1), []byte("hello\nworld"), 0644)
		os.WriteFile(filepath.Join(tmpDir, f2), []byte("foo\nbar"), 0644)

		args, _ := json.Marshal(map[string]any{
			"edits": []map[string]any{
				{
					"file_path":  f1,
					"old_string": "world",
					"new_string": "ion",
				},
				{
					"file_path":  f2,
					"old_string": "bar",
					"new_string": "baz",
				},
			},
		})

		res, err := m.Execute(context.Background(), string(args))
		if err != nil {
			t.Fatalf("multi_edit failed: %v", err)
		}
		if !strings.Contains(res, "Checkpoint: ") {
			t.Fatalf("multi_edit result missing checkpoint id: %q", res)
		}

		// Verify content
		c1, _ := os.ReadFile(filepath.Join(tmpDir, f1))
		if string(c1) != "hello\nion" {
			t.Errorf("f1 content mismatch: %q", string(c1))
		}

		// Verify diff output
		if !strings.Contains(res, "--- a/file1.txt") || !strings.Contains(res, "+++ b/file1.txt") {
			t.Errorf("diff for f1 missing in result: %q", res)
		}
		if !strings.Contains(res, "-world") || !strings.Contains(res, "+ion") {
			t.Errorf("hunk for f1 missing in result: %q", res)
		}
		if !strings.Contains(res, "--- a/file2.txt") || !strings.Contains(res, "-bar") || !strings.Contains(res, "+baz") {
			t.Errorf("diff for f2 missing in result: %q", res)
		}

		emptyArgs, _ := json.Marshal(map[string]any{"edits": []map[string]any{}})
		if _, err := m.Execute(context.Background(), string(emptyArgs)); err == nil {
			t.Fatal("expected empty multi_edit to fail")
		}

		badArgs, _ := json.Marshal(map[string]any{
			"edits": []map[string]any{
				{
					"file_path":  f1,
					"old_string": "",
					"new_string": "x",
				},
			},
		})
		if _, err := m.Execute(context.Background(), string(badArgs)); err == nil {
			t.Fatal("expected multi_edit with empty old_string to fail")
		}
	})

	t.Run("List", func(t *testing.T) {
		l := &List{FileTool: *newTestFileTool(t, tmpDir)}
		os.Mkdir(filepath.Join(tmpDir, "subdir"), 0755)
		os.WriteFile(filepath.Join(tmpDir, "file.txt"), []byte("hi"), 0644)

		args := `{"path": "."}`
		res, err := l.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("list failed: %v", err)
		}

		if !strings.Contains(res, "subdir/") {
			t.Errorf("expected list to contain subdir/, got %q", res)
		}
		if !strings.Contains(res, "file.txt") {
			t.Errorf("expected list to contain file.txt, got %q", res)
		}

		if _, err := l.Execute(context.Background(), `{"path":`); err == nil {
			t.Fatal("expected invalid list JSON to fail")
		}
	})
}
