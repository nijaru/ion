package memory

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

func TestStoreScopesAndSearch(t *testing.T) {
	ctx := context.Background()
	root := t.TempDir()
	scopeA := filepath.Join(root, "a")
	scopeB := filepath.Join(root, "b")
	if err := os.MkdirAll(scopeA, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(scopeB, 0o700); err != nil {
		t.Fatal(err)
	}
	store, err := Open(filepath.Join(root, "memory.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()

	first, err := store.Add(ctx, scopeA, "Use the stable API for the agent loop", "architecture,go")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.Add(ctx, scopeB, "A different workspace note", "other"); err != nil {
		t.Fatal(err)
	}

	rows, err := store.Search(ctx, scopeA, "STABLE", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].ID != first.ID || rows[0].Scope != mustRealPath(t, scopeA) {
		t.Fatalf("unexpected scope search result: %+v", rows)
	}
	rows, err = store.Search(ctx, scopeA, "", 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected bounded search, got %d rows", len(rows))
	}
	rows, err = store.Search(ctx, scopeB, "stable", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 0 {
		t.Fatalf("scope leaked into search: %+v", rows)
	}
}

func TestStoreDeleteRestoreAndAudit(t *testing.T) {
	ctx := context.Background()
	scope := t.TempDir()
	store, err := Open(filepath.Join(t.TempDir(), "memory.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()

	record, err := store.Add(ctx, scope, "Keep the session tree authoritative", "design")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.Delete(ctx, scope, record.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Delete(ctx, scope, record.ID); !errors.Is(err, ErrAlreadyDeleted) {
		t.Fatalf("second delete error = %v, want ErrAlreadyDeleted", err)
	}
	active, err := store.Search(ctx, scope, "", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(active) != 0 {
		t.Fatalf("deleted record remained active: %+v", active)
	}
	all, err := store.List(ctx, scope, true, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(all) != 1 || all[0].DeletedAt == nil {
		t.Fatalf("deleted record missing from audit view: %+v", all)
	}

	if _, err := store.Restore(ctx, scope, record.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Restore(ctx, scope, record.ID); !errors.Is(err, ErrAlreadyActive) {
		t.Fatalf("second restore error = %v, want ErrAlreadyActive", err)
	}
	active, err = store.Search(ctx, scope, "authoritative", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(active) != 1 || active[0].DeletedAt != nil {
		t.Fatalf("restored record missing: %+v", active)
	}

	audit, err := store.Audit(ctx, scope, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(audit) != 3 {
		t.Fatalf("audit entries = %d, want 3: %+v", len(audit), audit)
	}
	for i, want := range []string{"restore", "delete", "add"} {
		if audit[i].Operation != want || audit[i].MemoryID != record.ID {
			t.Fatalf("audit[%d] = %+v, want operation %q for %q", i, audit[i], want, record.ID)
		}
	}
}

func TestStorePersistsAcrossOpen(t *testing.T) {
	ctx := context.Background()
	scope := t.TempDir()
	path := filepath.Join(t.TempDir(), "memory.db")
	store, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	record, err := store.Add(ctx, scope, "This survives a process restart", "durable")
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	store, err = Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	rows, err := store.Search(ctx, scope, "restart", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].ID != record.ID {
		t.Fatalf("reopened rows = %+v, want %q", rows, record.ID)
	}
}

func TestStoreValidatesBoundsAndNotFound(t *testing.T) {
	ctx := context.Background()
	scope := t.TempDir()
	store, err := Open(filepath.Join(t.TempDir(), "memory.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()

	if _, err := store.Add(ctx, scope, strings.Repeat("x", MaxContentBytes+1), ""); err == nil {
		t.Fatal("oversized content was accepted")
	}
	if _, err := store.Add(ctx, scope, "valid", strings.Repeat("x", MaxTagsBytes+1)); err == nil {
		t.Fatal("oversized tags were accepted")
	}
	if _, err := store.Search(ctx, scope, strings.Repeat("x", MaxQueryBytes+1), 10); err == nil {
		t.Fatal("oversized query was accepted")
	}
	if _, err := store.Delete(ctx, scope, "missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing delete error = %v, want ErrNotFound", err)
	}
	if _, err := store.Restore(ctx, filepath.Join(scope, "missing"), "missing"); err == nil {
		t.Fatal("non-directory scope was accepted")
	}
}

func TestStoreConcurrentAdds(t *testing.T) {
	ctx := context.Background()
	scope := t.TempDir()
	store, err := Open(filepath.Join(t.TempDir(), "memory.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()

	const count = 12
	var wg sync.WaitGroup
	errs := make(chan error, count)
	for i := 0; i < count; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			_, err := store.Add(ctx, scope, "concurrent note", "test")
			if err != nil {
				errs <- err
			}
		}(i)
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Fatal(err)
	}
	rows, err := store.List(ctx, scope, false, count)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != count {
		t.Fatalf("concurrent add count = %d, want %d", len(rows), count)
	}
}

func mustRealPath(t *testing.T, path string) string {
	t.Helper()
	real, err := filepath.EvalSymlinks(path)
	if err != nil {
		t.Fatal(err)
	}
	return real
}
