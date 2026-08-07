package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	ionskills "github.com/nijaru/ion/internal/skills"
	"github.com/nijaru/ion/tool"
)

func TestSkillDirsForRuntimeGatesProjectSkills(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	project := t.TempDir()

	untrusted, err := skillDirsForRuntime("")
	if err != nil {
		t.Fatalf("untrusted skill dirs: %v", err)
	}
	if len(untrusted) != 1 || untrusted[0] != filepath.Join(home, ".ion", "skills") {
		t.Fatalf("untrusted skill dirs = %#v", untrusted)
	}

	trusted, err := skillDirsForRuntime(project)
	if err != nil {
		t.Fatalf("trusted skill dirs: %v", err)
	}
	want := []string{filepath.Join(home, ".ion", "skills"), filepath.Join(project, ".ion", "skills")}
	if len(trusted) != len(want) || trusted[0] != want[0] || trusted[1] != want[1] {
		t.Fatalf("trusted skill dirs = %#v, want %#v", trusted, want)
	}
}

func TestRuntimeSkillResourcesHonorProjectTrust(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	project := t.TempDir()

	writeRuntimeSkill := func(path, name string) {
		t.Helper()
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		content := "---\nname: " + name + "\ndescription: test skill\n---\nInstructions.\n"
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	globalSkill := filepath.Join(home, ".ion", "skills", "global-skill", "SKILL.md")
	projectSkill := filepath.Join(project, ".ion", "skills", "project-skill", "SKILL.md")
	writeRuntimeSkill(globalSkill, "global-skill")
	writeRuntimeSkill(projectSkill, "project-skill")
	duplicateGlobal := filepath.Join(home, ".ion", "skills", "duplicate", "SKILL.md")
	duplicateProject := filepath.Join(project, ".ion", "skills", "duplicate", "SKILL.md")
	writeRuntimeSkill(duplicateGlobal, "duplicate")
	writeRuntimeSkill(duplicateProject, "duplicate")
	if err := os.WriteFile(
		duplicateProject,
		[]byte("---\nname: duplicate\ndescription: project version\n---\nProject instructions.\n"),
		0o644,
	); err != nil {
		t.Fatal(err)
	}
	untrustedDirs, err := skillDirsForRuntime("")
	if err != nil {
		t.Fatal(err)
	}
	untrustedPrompt, err := ionskills.FormatSkillsForPrompt(untrustedDirs...)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(untrustedPrompt, "global-skill") || strings.Contains(untrustedPrompt, "project-skill") {
		t.Fatalf("untrusted skills prompt = %q", untrustedPrompt)
	}

	trustedDirs, err := skillDirsForRuntime(project)
	if err != nil {
		t.Fatal(err)
	}
	trustedPrompt, err := ionskills.FormatSkillsForPrompt(trustedDirs...)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(trustedPrompt, "global-skill") || !strings.Contains(trustedPrompt, "project-skill") {
		t.Fatalf("trusted skills prompt = %q", trustedPrompt)
	}
	if !strings.Contains(trustedPrompt, "project version") ||
		!strings.Contains(trustedPrompt, filepath.Join(project, ".ion", "skills", "duplicate", "SKILL.md")) {
		t.Fatalf("trusted duplicate skill did not use project source: %q", trustedPrompt)
	}
	detail, err := ionskills.Read(trustedDirs, "duplicate")
	if err != nil {
		t.Fatal(err)
	}
	if detail.Instructions != "Project instructions." {
		t.Fatalf("duplicate skill instructions = %q, want project source", detail.Instructions)
	}
}

func TestRuntimeCodingToolsConfigAppliesCredentialEnvironmentPolicy(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "secret")
	t.Setenv("ION_RUNTIME_VISIBLE", "visible")

	codingConfig, err := runtimeCodingToolsConfig(
		&config.Config{
			Provider: "openai",
			ToolEnv:  "inherit_without_provider_keys",
		},
		t.TempDir(),
		"",
		nil,
	)
	if err != nil {
		t.Fatalf("runtimeCodingToolsConfig error = %v", err)
	}

	registry := tool.NewRegistry()
	if err := tool.RegisterCodingTools(registry, codingConfig); err != nil {
		t.Fatalf("RegisterCodingTools error = %v", err)
	}
	bash, ok := registry.Get("bash")
	if !ok {
		t.Fatal("bash was not registered")
	}
	output, err := bash.Execute(
		context.Background(),
		`{"command":"printf '%s:%s' \"$OPENAI_API_KEY\" \"$ION_RUNTIME_VISIBLE\""}`,
	)
	if err != nil {
		t.Fatalf("bash execute error = %v", err)
	}
	if got, want := strings.TrimSpace(output), ":visible"; got != want {
		t.Fatalf("bash environment = %q, want provider key stripped and normal env preserved", got)
	}
}

func TestRuntimeCodingToolsConfigInheritsEnvironmentByDefault(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "secret")
	t.Setenv("ION_RUNTIME_VISIBLE", "not-allowlisted")

	codingConfig, err := runtimeCodingToolsConfig(&config.Config{}, t.TempDir(), "", nil)
	if err != nil {
		t.Fatalf("runtimeCodingToolsConfig error = %v", err)
	}
	registry := tool.NewRegistry()
	if err := tool.RegisterCodingTools(registry, codingConfig); err != nil {
		t.Fatalf("RegisterCodingTools error = %v", err)
	}
	bash, ok := registry.Get("bash")
	if !ok {
		t.Fatal("bash was not registered")
	}
	output, err := bash.Execute(
		context.Background(),
		`{"command":"printf '%s:%s' \"$OPENAI_API_KEY\" \"$ION_RUNTIME_VISIBLE\""}`,
	)
	if err != nil {
		t.Fatalf("bash execute error = %v", err)
	}
	if got, want := strings.TrimSpace(output), "secret:not-allowlisted"; got != want {
		t.Fatalf("default bash environment = %q, want inherited host environment", got)
	}
}

func TestRuntimeCodingToolsConfigGatesSkillRegistration(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	workdir := t.TempDir()

	tests := []struct {
		name       string
		cfg        config.Config
		wantSkill  bool
		wantActive bool
	}{
		{name: "default off", cfg: config.Config{}, wantSkill: false, wantActive: false},
		{name: "read enabled", cfg: config.Config{SkillTools: "read"}, wantSkill: true, wantActive: true},
		{name: "all mode", cfg: config.Config{ToolMode: "all"}, wantSkill: true, wantActive: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			codingConfig, err := runtimeCodingToolsConfig(&tt.cfg, workdir, "", nil)
			if err != nil {
				t.Fatalf("runtimeCodingToolsConfig error = %v", err)
			}
			registry := tool.NewRegistry()
			if err := tool.RegisterCodingTools(registry, codingConfig); err != nil {
				t.Fatalf("RegisterCodingTools error = %v", err)
			}
			_, registered := registry.Get("read_skill")
			if registered != tt.wantSkill {
				t.Fatalf("read_skill registered = %v, want %v", registered, tt.wantSkill)
			}
			active := activeToolNamesForModeWithSkills(
				registry,
				tt.cfg.ActiveToolMode(),
				tt.cfg.SkillToolMode() == "read",
			)
			containsSkill := false
			for _, name := range active {
				if name == "read_skill" {
					containsSkill = true
					break
				}
			}
			if containsSkill != tt.wantActive {
				t.Fatalf("active tools = %#v, read_skill present = %v, want %v", active, containsSkill, tt.wantActive)
			}
		})
	}
}

func TestActiveToolNamesForModeWithSkillsKeepsSkillDeferredWhenDisabled(t *testing.T) {
	registry := tool.NewRegistry()
	registry.Register(
		tool.Func(
			"read_skill",
			"read a skill",
			map[string]any{"type": "object"},
			func(context.Context, string) (string, error) {
				return "", nil
			},
		),
	)

	active := activeToolNamesForModeWithSkills(registry, "coding", false)
	for _, name := range active {
		if name == "read_skill" {
			t.Fatalf("disabled skill tool was active: %#v", active)
		}
	}
}
