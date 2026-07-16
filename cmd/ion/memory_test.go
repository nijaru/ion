package main

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	ionmemory "github.com/nijaru/ion/memory"
)

func TestTUIMemoryControllerRoundTripsWorkspaceNotes(t *testing.T) {
	path := filepath.Join(t.TempDir(), "memory.db")
	scope := t.TempDir()
	store, err := ionmemory.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	record, err := store.Add(context.Background(), scope, "controller note", "test")
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	controller := tuiMemoryController{path: path, scope: scope}
	rows, err := controller.Search(context.Background(), "controller", false, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].ID != record.ID || rows[0].Deleted {
		t.Fatalf("controller search = %+v", rows)
	}
	if err := controller.Delete(context.Background(), record.ID); err != nil {
		t.Fatal(err)
	}
	rows, err = controller.Search(context.Background(), "", false, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 0 {
		t.Fatalf("deleted controller record remained active: %+v", rows)
	}
	rows, err = controller.Search(context.Background(), "", true, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || !rows[0].Deleted {
		t.Fatalf("controller all = %+v", rows)
	}
	if err := controller.Restore(context.Background(), record.ID); err != nil {
		t.Fatal(err)
	}
}

func TestDefaultMemoryPathUsesIonDataDir(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	want := filepath.Join(home, ".ion", "data", "memory.db")
	got, err := defaultMemoryPath()
	if err != nil {
		t.Fatal(err)
	}
	if got != want {
		t.Fatalf("memory path = %q, want %q", got, want)
	}
	if _, err := os.Stat(filepath.Dir(got)); !os.IsNotExist(err) {
		t.Fatalf("default path unexpectedly created data directory: %v", err)
	}
}
