package instructions

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBasePromptContainsOperatingPolicy(t *testing.T) {
	prompt := BasePrompt()
	for _, want := range []string{
		"You are ion, a terminal coding agent.",
		"Treat project instruction files as authoritative within their scope.",
		"Inspect the relevant context first.",
		"After editing files, run relevant verification commands when feasible.",
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("base prompt missing %q: %q", want, prompt)
		}
	}
}

func TestBuildSystemPromptUsesDefaultContextAndRuntimeMetadata(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("project rules"), 0o644); err != nil {
		t.Fatalf("write AGENTS: %v", err)
	}

	prompt, err := BuildSystemPrompt("", "", "<available_skills>\nskill\n</available_skills>", root, root)
	if err != nil {
		t.Fatalf("BuildSystemPrompt: %v", err)
	}
	for _, want := range []string{
		"You are ion, a terminal coding agent.",
		"<project_context>",
		"project rules",
		"<available_skills>\nskill\n</available_skills>",
		"Current date: ",
		"Current working directory: " + filepath.ToSlash(root),
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("system prompt missing %q: %q", want, prompt)
		}
	}
	if strings.Index(prompt, "You are ion") > strings.Index(prompt, "<project_context>") {
		t.Fatalf("project context appears before base prompt: %q", prompt)
	}
	if strings.Index(prompt, "<project_context>") > strings.Index(prompt, "<available_skills>") {
		t.Fatalf("skills appear before project context: %q", prompt)
	}
	if strings.Index(prompt, "<available_skills>") > strings.Index(prompt, "Current date:") {
		t.Fatalf("runtime metadata appears before skills: %q", prompt)
	}
}

func TestBuildSystemPromptOverrideKeepsAppendAndProjectContext(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("trusted project rules"), 0o644); err != nil {
		t.Fatalf("write AGENTS: %v", err)
	}

	prompt, err := BuildSystemPrompt("custom policy", "extra policy", "", root, root)
	if err != nil {
		t.Fatalf("BuildSystemPrompt: %v", err)
	}
	for _, want := range []string{"custom policy", "extra policy", "trusted project rules"} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("system prompt missing %q: %q", want, prompt)
		}
	}
	if strings.Contains(prompt, "You are ion, a terminal coding agent.") {
		t.Fatalf("custom prompt unexpectedly contains default policy: %q", prompt)
	}
	if strings.Index(prompt, "extra policy") > strings.Index(prompt, "<project_context>") {
		t.Fatalf("project context should follow custom append: %q", prompt)
	}
}

func TestBuildSystemPromptReadsPromptFiles(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	overridePath := filepath.Join(root, "system.md")
	appendPath := filepath.Join(root, "append.md")
	if err := os.WriteFile(overridePath, []byte("file policy\n"), 0o644); err != nil {
		t.Fatalf("write override: %v", err)
	}
	if err := os.WriteFile(appendPath, []byte("file append\n"), 0o644); err != nil {
		t.Fatalf("write append: %v", err)
	}

	prompt, err := BuildSystemPrompt(overridePath, appendPath, "", root, root)
	if err != nil {
		t.Fatalf("BuildSystemPrompt: %v", err)
	}
	if !strings.Contains(prompt, "file policy") || !strings.Contains(prompt, "file append") {
		t.Fatalf("prompt missing file contents: %q", prompt)
	}
	if strings.Contains(prompt, overridePath) || strings.Contains(prompt, appendPath) {
		t.Fatalf("prompt used file paths instead of contents: %q", prompt)
	}
}

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
	if err := os.WriteFile(
		filepath.Join(root, "services", "AGENTS.md"),
		[]byte("services instructions"),
		0o644,
	); err != nil {
		t.Fatalf("write services AGENTS: %v", err)
	}
	if err := os.WriteFile(filepath.Join(nested, "AGENTS.md"), []byte("api instructions"), 0o644); err != nil {
		t.Fatalf("write api AGENTS: %v", err)
	}

	layers, err := LoadInstructionLayers(nested, root)
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

func TestLoadInstructionLayersStopsAtTrustedProjectRoot(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	parent := t.TempDir()
	root := filepath.Join(parent, "project")
	nested := filepath.Join(root, "nested")
	if err := os.MkdirAll(nested, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(parent, "AGENTS.md"), []byte("outside trust root"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("inside trust root"), 0o644); err != nil {
		t.Fatal(err)
	}

	layers, err := LoadInstructionLayers(nested, root)
	if err != nil {
		t.Fatal(err)
	}
	if len(layers) != 1 || layers[0].Content != "inside trust root" {
		t.Fatalf("layers = %#v, want only trusted project content", layers)
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

	layers, err := LoadInstructionLayers(root, root)
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
	if err := os.WriteFile(filepath.Join(root, "CLAUDE.MD"), []byte("claude upper"), 0o644); err != nil {
		t.Fatalf("write CLAUDE.MD: %v", err)
	}

	layers, err := LoadInstructionLayers(root, root)
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

	out, err := BuildInstructions("base rules", root, root)
	if err != nil {
		t.Fatalf("BuildInstructions: %v", err)
	}
	if !strings.Contains(out, "base rules") {
		t.Fatalf("instructions missing base rules: %q", out)
	}
	if !strings.Contains(out, "<project_context>") {
		t.Fatalf("instructions missing project_context XML tag: %q", out)
	}
	if !strings.Contains(out, "<project_instructions path=") {
		t.Fatalf("instructions missing project_instructions path XML tag: %q", out)
	}
	if !strings.Contains(out, "project rules") {
		t.Fatalf("instructions missing project content: %q", out)
	}
}

func TestBuildInstructionsSkipsUntrustedProjectLayers(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("untrusted project rules"), 0o644); err != nil {
		t.Fatalf("write AGENTS: %v", err)
	}

	out, err := BuildInstructions("base rules", root, "")
	if err != nil {
		t.Fatalf("BuildInstructions: %v", err)
	}
	if strings.Contains(out, "untrusted project rules") || strings.Contains(out, "<project_context>") {
		t.Fatalf("untrusted project instructions were loaded: %q", out)
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

	layers, err := LoadInstructionLayers(nested, root)
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

func TestLoadInstructionLayersSurfacesInstructionReadErrors(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, "AGENTS.md"), 0o755); err != nil {
		t.Fatalf("mkdir AGENTS: %v", err)
	}

	_, err := LoadInstructionLayers(root, root)
	if err == nil || !strings.Contains(err.Error(), "read") || !strings.Contains(err.Error(), "AGENTS.md") {
		t.Fatalf("LoadInstructionLayers error = %v, want AGENTS.md read error", err)
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

	layers, err := LoadInstructionLayers(root, root)
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
