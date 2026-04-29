package canto

import (
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

func TestBaseInstructionsUseExplicitSections(t *testing.T) {
	prompt := baseInstructions()

	required := []string{
		"## Identity",
		"## Core Mandates",
		"## Workflow",
		"## Tool and Approval Policy",
		"## Response Style",
		"You are ion, a terminal coding agent.",
		"Treat project instruction files as authoritative within their scope.",
		"After editing files, run relevant verification commands when feasible.",
		"Do not revert user changes, commit, or perform destructive operations unless the user explicitly asks.",
	}
	for _, want := range required {
		if !strings.Contains(prompt, want) {
			t.Fatalf("prompt missing %q\n%s", want, prompt)
		}
	}
}

func TestBaseInstructionsAvoidStaleBrandingAndRuntimeSpecifics(t *testing.T) {
	prompt := baseInstructions()

	forbidden := []string{
		"elite",
		"Canto framework",
		"Always prefer modern Go",
		"go test ./...",
		"high-fidelity verification loop",
		"`compact` tool",
		"## Runtime Context",
		"Platform:",
		"Working directory:",
	}
	for _, bad := range forbidden {
		if strings.Contains(prompt, bad) {
			t.Fatalf("prompt unexpectedly contains %q\n%s", bad, prompt)
		}
	}
}

func TestRuntimeInstructionsAreSeparateFromCorePolicy(t *testing.T) {
	now := time.Date(2026, time.March, 27, 0, 0, 0, 0, time.UTC)
	cwd := filepath.Join(t.TempDir(), "repo")
	prompt := runtimeInstructions(cwd, now)

	required := []string{
		"## Runtime Context",
		"Working directory: " + cwd,
		"Platform: " + runtime.GOOS + "/" + runtime.GOARCH,
		"Date: 2026-03-27",
		"Git repository: no",
	}
	for _, want := range required {
		if !strings.Contains(prompt, want) {
			t.Fatalf("runtime instructions missing %q\n%s", want, prompt)
		}
	}
}

func TestBuildInstructionsIncludesCoreAndRuntimeSections(t *testing.T) {
	now := time.Date(2026, time.March, 27, 0, 0, 0, 0, time.UTC)
	prompt := buildInstructions("/tmp/project", now)

	required := []string{
		"## Identity",
		"## Runtime Context",
	}
	for _, want := range required {
		if !strings.Contains(prompt, want) {
			t.Fatalf("combined instructions missing %q\n%s", want, prompt)
		}
	}
}
