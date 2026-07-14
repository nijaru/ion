package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestSessionDirOverride(t *testing.T) {
	value := " /tmp/ion-sessions "
	flags := cliFlags{sessionDirFlag: &value}
	if got := flags.sessionDirOverride(); got != "/tmp/ion-sessions" {
		t.Fatalf("sessionDirOverride = %q", got)
	}
}

func TestOpenStartupStoreUsesCustomDirectory(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "nested", "sessions")
	store, err := openStartupStore(false, dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, "ion.db")); err != nil {
		t.Fatalf("custom session database: %v", err)
	}

	reopened, err := openStartupStore(false, dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := reopened.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestNoSessionIgnoresCustomDirectory(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "must-not-exist")
	store, err := openStartupStore(true, dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatalf("custom directory stat error = %v, want not exist", err)
	}
}

func TestResolveSessionDirExpandsHome(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	got, err := resolveSessionDir("~/sessions")
	if err != nil {
		t.Fatal(err)
	}
	if got != filepath.Join(home, "sessions") {
		t.Fatalf("resolved session dir = %q, want %q", got, filepath.Join(home, "sessions"))
	}
}
