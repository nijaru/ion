package workspace

import (
	"strings"
	"testing"
)

func TestRefFileReturnsStableIdentity(t *testing.T) {
	root, err := Open(t.TempDir())
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if err := root.WriteFile("notes.txt", []byte("hello from file"), 0o644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	ref, data, err := RefFile(t.Context(), root, "notes.txt")
	if err != nil {
		t.Fatalf("RefFile: %v", err)
	}
	if string(data) != "hello from file" {
		t.Fatalf("data = %q, want hello from file", string(data))
	}
	if ref.Path != "notes.txt" {
		t.Fatalf("Path = %q, want notes.txt", ref.Path)
	}
	if !strings.HasPrefix(ref.Digest, "sha256:") {
		t.Fatalf("Digest = %q, want sha256 prefix", ref.Digest)
	}
	if ref.Size != int64(len("hello from file")) {
		t.Fatalf("Size = %d, want %d", ref.Size, len("hello from file"))
	}
	if got := ref.Identity(); got != ref.Digest {
		t.Fatalf("Identity = %q, want %q", got, ref.Digest)
	}
}
