package main

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/backend"
)

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
	if strings.Contains(line, "Tools:") || strings.Contains(line, "registered") {
		t.Fatalf("line = %q, want no startup tool count", line)
	}
	if !strings.Contains(line, "Sandbox auto: bubblewrap") {
		t.Fatalf("line = %q, want sandbox notice", line)
	}
	if strings.Contains(line, "Bash env") {
		t.Fatalf("line = %q, want no environment posture in startup shell", line)
	}
}

func TestStartupToolLineOmitsDefaultToolCount(t *testing.T) {
	line := startupToolLine(toolSummaryBackend{surface: backend.ToolSurface{
		Count: 7,
		Names: []string{"bash", "read", "write", "edit", "list", "grep", "glob"},
	}})
	if line != "" {
		t.Fatalf("line = %q, want no default tool count", line)
	}
}
