package backend

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestLoadInstructionLayersWalksAncestorsToCWD(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, ".git"), 0o755); err != nil {
		t.Fatalf("mkdir .git: %v", err)
	}

	nested := filepath.Join(root, "services", "api")
	if err := os.MkdirAll(nested, 0o755); err != nil {
		t.Fatalf("mkdir nested: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("root instructions"), 0o644); err != nil {
		t.Fatalf("write root AGENTS: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "services", "AGENTS.md"), []byte("services instructions"), 0o644); err != nil {
		t.Fatalf("write services AGENTS: %v", err)
	}
	if err := os.WriteFile(filepath.Join(nested, "AGENTS.md"), []byte("api instructions"), 0o644); err != nil {
		t.Fatalf("write api AGENTS: %v", err)
	}

	layers, err := LoadInstructionLayers(nested)
	if err != nil {
		t.Fatalf("LoadInstructionLayers: %v", err)
	}
	if len(layers) != 3 {
		t.Fatalf("layers = %d, want 3", len(layers))
	}
	if layers[0].Content != "root instructions" {
		t.Fatalf("root layer = %#v", layers[0])
	}
	if layers[1].Content != "services instructions" {
		t.Fatalf("services layer = %#v", layers[1])
	}
	if layers[2].Content != "api instructions" {
		t.Fatalf("api layer = %#v", layers[2])
	}
}

func TestLoadInstructionLayersPrefersAgentsOverClaude(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, ".git"), 0o755); err != nil {
		t.Fatalf("mkdir .git: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("agents"), 0o644); err != nil {
		t.Fatalf("write AGENTS: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "CLAUDE.md"), []byte("claude"), 0o644); err != nil {
		t.Fatalf("write CLAUDE: %v", err)
	}

	layers, err := LoadInstructionLayers(root)
	if err != nil {
		t.Fatalf("LoadInstructionLayers: %v", err)
	}
	if len(layers) != 1 {
		t.Fatalf("layers = %d, want 1", len(layers))
	}
	if layers[0].Content != "agents" {
		t.Fatalf("layer content = %q, want agents", layers[0].Content)
	}
}

func TestLoadInstructionLayersSupportsCaseVariantsAndFallback(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, "AGENTS.md"), 0o755); err != nil {
		t.Fatalf("mkdir unreadable AGENTS candidate: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "CLAUDE.MD"), []byte("claude upper"), 0o644); err != nil {
		t.Fatalf("write CLAUDE.MD: %v", err)
	}

	layers, err := LoadInstructionLayers(root)
	if err != nil {
		t.Fatalf("LoadInstructionLayers: %v", err)
	}
	if len(layers) != 1 {
		t.Fatalf("layers = %d, want 1", len(layers))
	}
	if layers[0].Content != "claude upper" {
		t.Fatalf("layer content = %q, want claude upper", layers[0].Content)
	}
}

func TestBuildInstructionsIncludesProjectSection(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, ".git"), 0o755); err != nil {
		t.Fatalf("mkdir .git: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("project rules"), 0o644); err != nil {
		t.Fatalf("write AGENTS: %v", err)
	}

	out, err := BuildInstructions("base rules", root)
	if err != nil {
		t.Fatalf("BuildInstructions: %v", err)
	}
	if !strings.Contains(out, "base rules") {
		t.Fatalf("instructions missing base rules: %q", out)
	}
	if !strings.Contains(out, "## Project Instructions") {
		t.Fatalf("instructions missing project heading: %q", out)
	}
	if !strings.Contains(out, "project rules") {
		t.Fatalf("instructions missing project content: %q", out)
	}
}

func TestLoadInstructionLayersWithoutRepoWalksAncestors(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	nested := filepath.Join(root, "nested")
	if err := os.MkdirAll(nested, 0o755); err != nil {
		t.Fatalf("mkdir nested: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("root instructions"), 0o644); err != nil {
		t.Fatalf("write root AGENTS: %v", err)
	}
	if err := os.WriteFile(filepath.Join(nested, "AGENTS.md"), []byte("nested instructions"), 0o644); err != nil {
		t.Fatalf("write nested AGENTS: %v", err)
	}

	layers, err := LoadInstructionLayers(nested)
	if err != nil {
		t.Fatalf("LoadInstructionLayers: %v", err)
	}
	if len(layers) != 2 {
		t.Fatalf("layers = %d, want 2", len(layers))
	}
	if layers[0].Content != "root instructions" {
		t.Fatalf("root layer content = %q, want root instructions", layers[0].Content)
	}
	if layers[1].Content != "nested instructions" {
		t.Fatalf("nested layer content = %q, want nested instructions", layers[1].Content)
	}
}

func TestLoadInstructionLayersIncludesGlobalIonInstructionsFirst(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	globalDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(globalDir, 0o755); err != nil {
		t.Fatalf("mkdir global dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(globalDir, "AGENTS.md"), []byte("global instructions"), 0o644); err != nil {
		t.Fatalf("write global AGENTS: %v", err)
	}

	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("project instructions"), 0o644); err != nil {
		t.Fatalf("write project AGENTS: %v", err)
	}

	layers, err := LoadInstructionLayers(root)
	if err != nil {
		t.Fatalf("LoadInstructionLayers: %v", err)
	}
	if len(layers) != 2 {
		t.Fatalf("layers = %d, want 2", len(layers))
	}
	if layers[0].Content != "global instructions" {
		t.Fatalf("global layer content = %q, want global instructions", layers[0].Content)
	}
	if layers[1].Content != "project instructions" {
		t.Fatalf("project layer content = %q, want project instructions", layers[1].Content)
	}
}
