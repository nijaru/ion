package main

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
)

func TestLoadWorkspaceTrustReportsUntrusted(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	_, trusted, notice, err := loadWorkspaceTrust(filepath.Join(home, "repo"), &config.Config{})
	if err != nil {
		t.Fatalf("loadWorkspaceTrust returned error: %v", err)
	}
	if trusted {
		t.Fatal("workspace starts trusted")
	}
	if !strings.Contains(notice, "READ mode") {
		t.Fatalf("notice = %q, want READ mode warning", notice)
	}
	if !strings.Contains(notice, "Workspace is not trusted.") {
		t.Fatalf("notice = %q, want grammatical trust warning", notice)
	}
}

func TestLoadWorkspaceTrustCanBeDisabled(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	store, trusted, notice, err := loadWorkspaceTrust(filepath.Join(home, "repo"), &config.Config{WorkspaceTrust: "off"})
	if err != nil {
		t.Fatalf("loadWorkspaceTrust returned error: %v", err)
	}
	if store != nil {
		t.Fatal("disabled trust should not create a trust store")
	}
	if !trusted {
		t.Fatal("workspace trust off should treat workspace as eligible")
	}
	if notice != "" {
		t.Fatalf("notice = %q, want empty", notice)
	}
}

type toolSummaryBackend struct {
	backend.Backend
	surface backend.ToolSurface
}

func (b toolSummaryBackend) ToolSurface() backend.ToolSurface {
	return b.surface
}

func TestStartupToolLineReportsLazyTools(t *testing.T) {
	line := startupToolLine(toolSummaryBackend{surface: backend.ToolSurface{
		Count:       25,
		LazyEnabled: true,
		Sandbox:     "auto: bubblewrap",
	}})
	if !strings.Contains(line, "Search tools enabled") {
		t.Fatalf("line = %q, want search tools notice", line)
	}
	if !strings.Contains(line, "25 tools registered") {
		t.Fatalf("line = %q, want grammatical tool count", line)
	}
	if !strings.Contains(line, "Sandbox auto: bubblewrap") {
		t.Fatalf("line = %q, want sandbox notice", line)
	}
}
