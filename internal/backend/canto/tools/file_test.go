package tools

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestFileTools(t *testing.T) {
	tmpDir := t.TempDir()
	
	t.Run("Write and Read", func(t *testing.T) {
		w := &Write{FileTool: *NewFileTool(tmpDir)}
		r := &Read{FileTool: *NewFileTool(tmpDir)}
		
		filePath := "test.txt"
		content := "line 1\nline 2\nline 3"
		
		// Write
		writeArgs, _ := json.Marshal(map[string]any{
			"file_path": filePath,
			"content":   content,
		})
		_, err := w.Execute(context.Background(), string(writeArgs))
		if err != nil {
			t.Fatalf("write failed: %v", err)
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
	})

	t.Run("Edit", func(t *testing.T) {
		e := &Edit{FileTool: *NewFileTool(tmpDir)}
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
	})

	t.Run("MultiEdit", func(t *testing.T) {
		m := &MultiEdit{FileTool: *NewFileTool(tmpDir)}
		
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
	})

	t.Run("List", func(t *testing.T) {
		l := &List{FileTool: *NewFileTool(tmpDir)}
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
	})
}
