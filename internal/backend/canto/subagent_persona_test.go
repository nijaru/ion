package canto

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/subagents"
)

func TestLoadSubagentPersonasMergesCustomAgents(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "explorer.md"), []byte(`---
name: explorer
description: Custom explorer.
model: primary
tools: [read]
---
Custom prompt.
`), 0o600); err != nil {
		t.Fatalf("write persona: %v", err)
	}

	personas, err := loadSubagentPersonas(&config.Config{SubagentsPath: dir})
	if err != nil {
		t.Fatalf("loadSubagentPersonas returned error: %v", err)
	}
	if len(personas) != 3 {
		t.Fatalf("persona count = %d, want 3", len(personas))
	}
	found := false
	for _, persona := range personas {
		if persona.Name == "explorer" {
			found = true
			if persona.Description != "Custom explorer." {
				t.Fatalf("explorer description = %q, want custom", persona.Description)
			}
		}
	}
	if !found {
		t.Fatal("explorer persona not found")
	}
}

func TestValidateSubagentPersonaToolsFailsClosed(t *testing.T) {
	registry := tool.NewRegistry()
	registry.Register(&testTool{name: "read"})

	err := validateSubagentPersonaTools([]subagents.Persona{{
		Name:        "bad",
		Description: "bad",
		ModelSlot:   subagents.ModelSlotFast,
		Tools:       []string{"read", "missing"},
		Prompt:      "bad prompt",
	}}, registry)
	if err == nil {
		t.Fatal("validateSubagentPersonaTools returned nil error")
	}
}
