package workspace

import (
	"path/filepath"
	"testing"
)

func TestTrustStorePersistsWorkspaceTrust(t *testing.T) {
	path := filepath.Join(t.TempDir(), "trust.json")
	store := NewTrustStore(path)
	workspace := filepath.Join(t.TempDir(), "repo")

	trusted, err := store.IsTrusted(workspace)
	if err != nil {
		t.Fatalf("IsTrusted returned error: %v", err)
	}
	if trusted {
		t.Fatal("workspace starts trusted")
	}

	if err := store.Trust(workspace); err != nil {
		t.Fatalf("Trust returned error: %v", err)
	}
	trusted, err = NewTrustStore(path).IsTrusted(workspace)
	if err != nil {
		t.Fatalf("IsTrusted after reload returned error: %v", err)
	}
	if !trusted {
		t.Fatal("workspace is not trusted after Trust")
	}
}

func TestTrustStoreListSortsPaths(t *testing.T) {
	path := filepath.Join(t.TempDir(), "trust.json")
	store := NewTrustStore(path)
	if err := store.Trust("/tmp/z"); err != nil {
		t.Fatalf("trust z: %v", err)
	}
	if err := store.Trust("/tmp/a"); err != nil {
		t.Fatalf("trust a: %v", err)
	}

	paths, err := store.List()
	if err != nil {
		t.Fatalf("List returned error: %v", err)
	}
	if len(paths) != 2 || paths[0] != "/tmp/a" || paths[1] != "/tmp/z" {
		t.Fatalf("paths = %#v, want sorted", paths)
	}
}
