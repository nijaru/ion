package workspace

import (
	"slices"
	"testing"
)

func TestTrigramIndexUpsertSearchAndUpdate(t *testing.T) {
	index := NewSearchIndex()

	first := ContentRef{
		Path:   "notes/first.txt",
		Digest: "sha256:first",
		Size:   int64(len("alpha beta")),
	}
	if err := index.Upsert(t.Context(), first, []byte("alpha beta")); err != nil {
		t.Fatalf("Upsert(first): %v", err)
	}

	hits, err := index.Search(t.Context(), "alpha", 10)
	if err != nil {
		t.Fatalf("Search(alpha): %v", err)
	}
	if len(hits) != 1 {
		t.Fatalf("Search(alpha) hits = %d, want 1", len(hits))
	}
	if hits[0].Ref.Path != first.Path {
		t.Fatalf("Search(alpha)[0].Path = %q, want %q", hits[0].Ref.Path, first.Path)
	}
	if hits[0].Ref.Digest != first.Digest {
		t.Fatalf("Search(alpha)[0].Digest = %q, want %q", hits[0].Ref.Digest, first.Digest)
	}

	updated := ContentRef{
		Path:   "notes/first.txt",
		Digest: "sha256:second",
		Size:   int64(len("gamma delta")),
	}
	if err := index.Upsert(t.Context(), updated, []byte("gamma delta")); err != nil {
		t.Fatalf("Upsert(updated): %v", err)
	}

	hits, err = index.Search(t.Context(), "alpha", 10)
	if err != nil {
		t.Fatalf("Search(alpha after update): %v", err)
	}
	if len(hits) != 0 {
		t.Fatalf("Search(alpha after update) hits = %d, want 0", len(hits))
	}

	hits, err = index.Search(t.Context(), "gamma", 10)
	if err != nil {
		t.Fatalf("Search(gamma): %v", err)
	}
	if len(hits) != 1 {
		t.Fatalf("Search(gamma) hits = %d, want 1", len(hits))
	}
	if hits[0].Ref.Digest != updated.Digest {
		t.Fatalf("Search(gamma)[0].Digest = %q, want %q", hits[0].Ref.Digest, updated.Digest)
	}

	if err := index.Delete(t.Context(), updated.Path); err != nil {
		t.Fatalf("Delete: %v", err)
	}
	hits, err = index.Search(t.Context(), "gamma", 10)
	if err != nil {
		t.Fatalf("Search(gamma after delete): %v", err)
	}
	if len(hits) != 0 {
		t.Fatalf("Search(gamma after delete) hits = %d, want 0", len(hits))
	}
}

func TestIndexWorkspaceIndexesWorkspaceFS(t *testing.T) {
	root, err := Open(t.TempDir())
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	if err := root.WriteFile("docs/readme.md", []byte("workspace search substrate"), 0o644); err != nil {
		t.Fatalf("WriteFile(readme): %v", err)
	}
	if err := root.WriteFile("src/main.go", []byte("package main\nfunc main() { println(\"index me\") }"), 0o644); err != nil {
		t.Fatalf("WriteFile(main): %v", err)
	}

	index := NewSearchIndex()
	count, err := IndexWorkspace(t.Context(), root, index)
	if err != nil {
		t.Fatalf("IndexWorkspace: %v", err)
	}
	if count != 2 {
		t.Fatalf("IndexWorkspace count = %d, want 2", count)
	}

	hits, err := index.Search(t.Context(), "substrate", 10)
	if err != nil {
		t.Fatalf("Search(substrate): %v", err)
	}
	if len(hits) != 1 {
		t.Fatalf("Search(substrate) hits = %d, want 1", len(hits))
	}
	if hits[0].Ref.Path != "docs/readme.md" {
		t.Fatalf("Search(substrate)[0].Path = %q, want docs/readme.md", hits[0].Ref.Path)
	}

	hits, err = index.Search(t.Context(), "main.go", 10)
	if err != nil {
		t.Fatalf("Search(main.go): %v", err)
	}
	if len(hits) != 1 {
		t.Fatalf("Search(main.go) hits = %d, want 1", len(hits))
	}
	if hits[0].Ref.Path != "src/main.go" {
		t.Fatalf("Search(main.go)[0].Path = %q, want src/main.go", hits[0].Ref.Path)
	}

	hits, err = index.Search(t.Context(), "go", 10)
	if err != nil {
		t.Fatalf("Search(go): %v", err)
	}
	if len(hits) != 1 {
		t.Fatalf("Search(go) hits = %d, want 1", len(hits))
	}
	if hits[0].Ref.Path != "src/main.go" {
		t.Fatalf("Search(go)[0].Path = %q, want src/main.go", hits[0].Ref.Path)
	}
}

func TestIndexWorkspaceIndexesOverlayFiles(t *testing.T) {
	root, err := Open(t.TempDir())
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = root.Close() })

	overlay := NewOverlayFS(root)
	if err := overlay.WriteFile("scratch/spec.txt", []byte("speculative overlay note"), 0o644); err != nil {
		t.Fatalf("overlay WriteFile: %v", err)
	}

	index := NewSearchIndex()
	count, err := IndexWorkspace(t.Context(), overlay, index)
	if err != nil {
		t.Fatalf("IndexWorkspace overlay: %v", err)
	}
	if count != 1 {
		t.Fatalf("IndexWorkspace overlay count = %d, want 1", count)
	}

	hits, err := index.Search(t.Context(), "speculative", 10)
	if err != nil {
		t.Fatalf("Search(speculative): %v", err)
	}
	if len(hits) != 1 || hits[0].Ref.Path != "scratch/spec.txt" {
		t.Fatalf("overlay hits = %#v, want scratch/spec.txt", hits)
	}
}

func TestIndexWorkspaceIndexesMountedFilesystems(t *testing.T) {
	base, err := Open(t.TempDir())
	if err != nil {
		t.Fatalf("Open base: %v", err)
	}
	t.Cleanup(func() { _ = base.Close() })
	if err := base.WriteFile("base.txt", []byte("base workspace note"), 0o644); err != nil {
		t.Fatalf("base WriteFile: %v", err)
	}

	mounted, err := Open(t.TempDir())
	if err != nil {
		t.Fatalf("Open mounted: %v", err)
	}
	t.Cleanup(func() { _ = mounted.Close() })
	if err := mounted.WriteFile("note.txt", []byte("mounted memory note"), 0o644); err != nil {
		t.Fatalf("mounted WriteFile: %v", err)
	}

	multi := NewMultiFS(base)
	multi.Mount("memory", mounted)

	index := NewSearchIndex()
	count, err := IndexWorkspace(t.Context(), multi, index)
	if err != nil {
		t.Fatalf("IndexWorkspace multi: %v", err)
	}
	if count != 2 {
		t.Fatalf("IndexWorkspace multi count = %d, want 2", count)
	}

	hits, err := index.Search(t.Context(), "mounted", 10)
	if err != nil {
		t.Fatalf("Search(mounted): %v", err)
	}
	if len(hits) != 1 || hits[0].Ref.Path != "memory/note.txt" {
		t.Fatalf("mounted hits = %#v, want memory/note.txt", hits)
	}
}

func TestTrigramIndexReturnsStablePathOrdering(t *testing.T) {
	index := NewSearchIndex()
	for _, file := range []ContentRef{
		{Path: "b.txt", Digest: "sha256:b", Size: 3},
		{Path: "a.txt", Digest: "sha256:a", Size: 3},
	} {
		if err := index.Upsert(t.Context(), file, []byte("shared term")); err != nil {
			t.Fatalf("Upsert(%s): %v", file.Path, err)
		}
	}

	hits, err := index.Search(t.Context(), "shared", 10)
	if err != nil {
		t.Fatalf("Search(shared): %v", err)
	}
	if len(hits) != 2 {
		t.Fatalf("Search(shared) hits = %d, want 2", len(hits))
	}
	got := []string{hits[0].Ref.Path, hits[1].Ref.Path}
	want := []string{"a.txt", "b.txt"}
	if !slices.Equal(got, want) {
		t.Fatalf("Search(shared) paths = %#v, want %#v", got, want)
	}
}
