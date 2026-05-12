package main

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
)

func TestLoadWorkspaceTrustIsDisabledDuringCoreStabilization(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	workspace := filepath.Join(home, "repo")

	store, trusted, notice, err := loadWorkspaceTrust(
		workspace,
		&config.Config{WorkspaceTrust: "prompt"},
	)
	if err != nil {
		t.Fatalf("loadWorkspaceTrust returned error: %v", err)
	}
	if store != nil {
		t.Fatal("workspace trust should not create a trust store")
	}
	if !trusted {
		t.Fatal("workspaces should be trusted by default")
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
		Environment: "inherit",
	}})
	if !strings.Contains(line, "Search tools enabled") {
		t.Fatalf("line = %q, want search tools notice", line)
	}
	if !strings.Contains(line, "Tools: 25 registered") {
		t.Fatalf("line = %q, want grammatical tool count", line)
	}
	if !strings.Contains(line, "Sandbox auto: bubblewrap") {
		t.Fatalf("line = %q, want sandbox notice", line)
	}
	if strings.Contains(line, "Bash env") {
		t.Fatalf("line = %q, want no environment posture in startup shell", line)
	}
}
