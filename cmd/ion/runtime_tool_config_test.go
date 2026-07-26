package main

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/tool"
)

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

func TestRuntimeCodingToolsConfigUsesSafeEnvironmentByDefault(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "secret")
	t.Setenv("ION_RUNTIME_VISIBLE", "not-allowlisted")

	codingConfig, err := runtimeCodingToolsConfig(&config.Config{}, t.TempDir(), nil)
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
	if got, want := strings.TrimSpace(output), ":"; got != want {
		t.Fatalf("default bash environment = %q, want both variables absent", got)
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
			codingConfig, err := runtimeCodingToolsConfig(&tt.cfg, workdir, nil)
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
