package tools

import (
	"slices"
	"testing"

	"github.com/nijaru/ion/tool"
)

func TestRegisterCodingToolsOwnsDefaultSurface(t *testing.T) {
	registry := tool.NewRegistry()
	if err := RegisterCodingTools(registry, CodingToolsConfig{
		Workdir:     t.TempDir(),
		Environment: NewEnvironmentPolicy(executorEnvironmentInherit, nil),
		SkillDirs:   []string{t.TempDir()},
	}); err != nil {
		t.Fatalf("RegisterCodingTools error = %v", err)
	}

	want := []string{"bash", "edit", "find", "grep", "ls", "read", "read_skill", "write"}
	if got := registry.Names(); !slices.Equal(got, want) {
		t.Fatalf("registered tools = %#v, want %#v", got, want)
	}
}

func TestRegisterCodingToolsRejectsNilRegistry(t *testing.T) {
	if err := RegisterCodingTools(nil, CodingToolsConfig{}); err == nil {
		t.Fatal("expected nil registry error")
	}
}
