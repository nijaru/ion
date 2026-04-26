package main

import (
	"path/filepath"
	"strings"
	"testing"
)

func TestLoadWorkspaceTrustReportsUntrusted(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	_, trusted, notice, err := loadWorkspaceTrust(filepath.Join(home, "repo"))
	if err != nil {
		t.Fatalf("loadWorkspaceTrust returned error: %v", err)
	}
	if trusted {
		t.Fatal("workspace starts trusted")
	}
	if !strings.Contains(notice, "READ mode") {
		t.Fatalf("notice = %q, want READ mode warning", notice)
	}
}
