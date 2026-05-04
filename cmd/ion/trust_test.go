package main

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
)

func TestLoadWorkspaceTrustPromptStartsUnknownWorkspaceReadOnly(t *testing.T) {
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
	if store == nil {
		t.Fatal("prompt workspace trust should create a trust store")
	}
	if trusted {
		t.Fatal("unknown prompt workspace should start untrusted")
	}
	if !strings.Contains(notice, "Run /trust") {
		t.Fatalf("notice = %q, want /trust guidance", notice)
	}

	if err := store.Trust(workspace); err != nil {
		t.Fatalf("trust workspace: %v", err)
	}
	_, trusted, notice, err = loadWorkspaceTrust(
		workspace,
		&config.Config{WorkspaceTrust: "prompt"},
	)
	if err != nil {
		t.Fatalf("reload workspace trust returned error: %v", err)
	}
	if !trusted {
		t.Fatal("trusted workspace should reload as trusted")
	}
	if notice != "" {
		t.Fatalf("trusted notice = %q, want empty", notice)
	}
}

func TestLoadWorkspaceTrustStrictStartsUnknownWorkspaceReadOnly(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	store, trusted, notice, err := loadWorkspaceTrust(
		filepath.Join(home, "repo"),
		&config.Config{WorkspaceTrust: "strict"},
	)
	if err != nil {
		t.Fatalf("loadWorkspaceTrust returned error: %v", err)
	}
	if store == nil {
		t.Fatal("strict workspace trust should create a trust store")
	}
	if trusted {
		t.Fatal("unknown strict workspace should start untrusted")
	}
	if !strings.Contains(notice, "managed outside this session") {
		t.Fatalf("notice = %q, want strict management guidance", notice)
	}
}

func TestLoadWorkspaceTrustCanBeDisabled(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	store, trusted, notice, err := loadWorkspaceTrust(
		filepath.Join(home, "repo"),
		&config.Config{WorkspaceTrust: "off"},
	)
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
