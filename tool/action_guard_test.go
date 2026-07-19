package tool

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestWriteRejectsActionPathSymlinkRetarget(t *testing.T) {
	root := t.TempDir()
	first := filepath.Join(root, "first.txt")
	second := filepath.Join(root, "second.txt")
	link := filepath.Join(root, "link.txt")
	if err := os.WriteFile(first, []byte("first"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(second, []byte("second"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(first, link); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	if err := os.Remove(link); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(second, link); err != nil {
		t.Fatal(err)
	}

	args, err := json.Marshal(map[string]any{"path": "link.txt", "content": "changed"})
	if err != nil {
		t.Fatal(err)
	}
	w := &Write{FileTool: *newTestFileTool(t, root)}
	_, err = w.Execute(WithActionPathGuard(t.Context(), []string{first}), string(args))
	if err == nil || !strings.Contains(err.Error(), "approved action target") {
		t.Fatalf("retargeted write error = %v, want approved-target rejection", err)
	}
	if content, readErr := os.ReadFile(second); readErr != nil || string(content) != "second" {
		t.Fatalf("retargeted target content = %q, read error = %v", content, readErr)
	}
}
