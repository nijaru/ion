package main

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/backend"
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
	}})
	if !strings.Contains(line, "search_tools enabled") {
		t.Fatalf("line = %q, want search_tools notice", line)
	}
}
