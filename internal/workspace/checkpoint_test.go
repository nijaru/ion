package workspace

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"
)

func TestCheckpointRestoreRestoresFilesAndRemovesCreatedPaths(t *testing.T) {
	workspacePath := t.TempDir()
	store := NewCheckpointStore(filepath.Join(t.TempDir(), "checkpoints"))

	if err := os.WriteFile(filepath.Join(workspacePath, "existing.txt"), []byte("before"), 0o644); err != nil {
		t.Fatalf("write existing: %v", err)
	}

	cp, err := store.Create(t.Context(), workspacePath, []string{"existing.txt", "created.txt"})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}

	if err := os.WriteFile(filepath.Join(workspacePath, "existing.txt"), []byte("after"), 0o644); err != nil {
		t.Fatalf("modify existing: %v", err)
	}
	if err := os.WriteFile(filepath.Join(workspacePath, "created.txt"), []byte("new"), 0o644); err != nil {
		t.Fatalf("create new: %v", err)
	}

	plan, err := store.AnalyzeRestore(t.Context(), cp)
	if err != nil {
		t.Fatalf("AnalyzeRestore: %v", err)
	}
	if len(plan.Conflicts) != 2 {
		t.Fatalf("conflicts = %#v, want 2", plan.Conflicts)
	}
	if _, err := store.Restore(t.Context(), cp, RestoreOptions{}); err == nil {
		t.Fatal("Restore without confirmation accepted destructive changes")
	}

	report, err := store.Restore(t.Context(), cp, RestoreOptions{AllowConflicts: true})
	if err != nil {
		t.Fatalf("Restore confirmed: %v", err)
	}
	if !slices.Contains(report.Restored, "existing.txt") {
		t.Fatalf("report restored = %#v, want existing.txt", report.Restored)
	}
	if !slices.Contains(report.Removed, "created.txt") {
		t.Fatalf("report removed = %#v, want created.txt", report.Removed)
	}

	data, err := os.ReadFile(filepath.Join(workspacePath, "existing.txt"))
	if err != nil {
		t.Fatalf("read restored: %v", err)
	}
	if string(data) != "before" {
		t.Fatalf("existing content = %q, want before", data)
	}
	if _, err := os.Stat(filepath.Join(workspacePath, "created.txt")); !os.IsNotExist(err) {
		t.Fatalf("created file still exists or stat failed: %v", err)
	}
}

func TestCheckpointRestoreHandlesBinaryAndNestedPaths(t *testing.T) {
	workspacePath := t.TempDir()
	store := NewCheckpointStore(filepath.Join(t.TempDir(), "checkpoints"))
	nested := filepath.Join(workspacePath, "dir")
	if err := os.MkdirAll(nested, 0o755); err != nil {
		t.Fatalf("mkdir nested: %v", err)
	}
	before := []byte{0x00, 0x01, 0xff, 0x41}
	if err := os.WriteFile(filepath.Join(nested, "blob.bin"), before, 0o600); err != nil {
		t.Fatalf("write binary: %v", err)
	}

	cp, err := store.Create(t.Context(), workspacePath, []string{"dir/blob.bin"})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if err := os.WriteFile(filepath.Join(nested, "blob.bin"), []byte("changed"), 0o600); err != nil {
		t.Fatalf("modify binary: %v", err)
	}
	if err := os.Chmod(filepath.Join(nested, "blob.bin"), 0o644); err != nil {
		t.Fatalf("change binary mode: %v", err)
	}

	if _, err := store.Restore(t.Context(), cp, RestoreOptions{AllowConflicts: true}); err != nil {
		t.Fatalf("Restore: %v", err)
	}
	data, err := os.ReadFile(filepath.Join(nested, "blob.bin"))
	if err != nil {
		t.Fatalf("read binary: %v", err)
	}
	if !slices.Equal(data, before) {
		t.Fatalf("binary content = %#v, want %#v", data, before)
	}
	info, err := os.Stat(filepath.Join(nested, "blob.bin"))
	if err != nil {
		t.Fatalf("stat restored binary: %v", err)
	}
	if got := info.Mode().Perm(); got != 0o600 {
		t.Fatalf("restored mode = %v, want 0600", got)
	}
}

func TestCheckpointRejectsEscapingPaths(t *testing.T) {
	store := NewCheckpointStore(filepath.Join(t.TempDir(), "checkpoints"))
	if _, err := store.Create(t.Context(), t.TempDir(), []string{"../outside.txt"}); err == nil {
		t.Fatal("Create accepted escaping path")
	}
}

func TestCheckpointLoadRejectsMismatchedID(t *testing.T) {
	workspacePath := t.TempDir()
	store := NewCheckpointStore(filepath.Join(t.TempDir(), "checkpoints"))
	cp, err := store.Create(t.Context(), workspacePath, []string{"missing.txt"})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}

	manifest := filepath.Join(store.path, cp.ID, checkpointManifestName)
	if err := os.WriteFile(manifest, []byte(`{"id":"other"}`), 0o600); err != nil {
		t.Fatalf("write manifest: %v", err)
	}
	if _, err := store.Load(cp.ID); err == nil {
		t.Fatal("Load accepted mismatched id")
	}
	if _, err := store.Load(".."); err == nil {
		t.Fatal("Load accepted a parent checkpoint path")
	}
}

func TestCheckpointListFiltersWorkspaceAndSortsNewestFirst(t *testing.T) {
	workspacePath := t.TempDir()
	otherWorkspace := t.TempDir()
	store := NewCheckpointStore(filepath.Join(t.TempDir(), "checkpoints"))

	first, err := store.Create(t.Context(), workspacePath, []string{"one.txt"})
	if err != nil {
		t.Fatalf("Create first: %v", err)
	}
	second, err := store.Create(t.Context(), workspacePath, []string{"two.txt", "three.txt"})
	if err != nil {
		t.Fatalf("Create second: %v", err)
	}
	if _, err := store.Create(t.Context(), otherWorkspace, []string{"other.txt"}); err != nil {
		t.Fatalf("Create other: %v", err)
	}

	// Make ordering deterministic without depending on the clock granularity.
	secondManifest := filepath.Join(store.path, second.ID, checkpointManifestName)
	data, err := os.ReadFile(secondManifest)
	if err != nil {
		t.Fatalf("read second manifest: %v", err)
	}
	second.CreatedAt = first.CreatedAt.Add(time.Minute)
	data, err = json.MarshalIndent(second, "", "  ")
	if err != nil {
		t.Fatalf("marshal second manifest: %v", err)
	}
	if err := os.WriteFile(secondManifest, append(data, '\n'), 0o600); err != nil {
		t.Fatalf("write second manifest: %v", err)
	}

	got, err := store.List(t.Context(), workspacePath, 10)
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("List returned %#v, want two workspace checkpoints", got)
	}
	if got[0].ID != second.ID || got[1].ID != first.ID {
		t.Fatalf("List order = %#v, want %q then %q", got, second.ID, first.ID)
	}
	if got[0].PathCount != 2 {
		t.Fatalf("PathCount = %d, want 2", got[0].PathCount)
	}

	if _, err := store.LoadForWorkspace(second.ID, otherWorkspace); err == nil {
		t.Fatal("LoadForWorkspace accepted a foreign checkpoint")
	}
}

func TestCheckpointListMissingStoreIsEmpty(t *testing.T) {
	store := NewCheckpointStore(filepath.Join(t.TempDir(), "missing"))
	got, err := store.List(t.Context(), t.TempDir(), 10)
	if err != nil {
		t.Fatalf("List missing store: %v", err)
	}
	if got != nil {
		t.Fatalf("List missing store = %#v, want nil", got)
	}
}
