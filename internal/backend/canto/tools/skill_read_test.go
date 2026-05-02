package tools

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestReadSkillTool(t *testing.T) {
	root := t.TempDir()
	writeTestSkill(t, filepath.Join(root, "review", "SKILL.md"), `---
name: review
description: Review changes.
allowed-tools: [read]
---
Inspect the diff.
`)

	tool := NewReadSkill([]string{root})
	spec := tool.Spec()
	if spec.Name != "read_skill" {
		t.Fatalf("spec name = %q, want read_skill", spec.Name)
	}
	out, err := tool.Execute(t.Context(), `{"name":"review"}`)
	if err != nil {
		t.Fatalf("Execute returned error: %v", err)
	}
	for _, want := range []string{
		"# review",
		"Review changes.",
		"Allowed tools: read",
		"Inspect the diff.",
	} {
		if !strings.Contains(out, want) {
			t.Fatalf("output missing %q:\n%s", want, out)
		}
	}
}

func TestReadSkillToolRequiresName(t *testing.T) {
	tool := NewReadSkill([]string{t.TempDir()})
	_, err := tool.Execute(t.Context(), `{"name":""}`)
	if err == nil || !strings.Contains(err.Error(), "name is required") {
		t.Fatalf("Execute error = %v, want name requirement", err)
	}
}

func writeTestSkill(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir skill dir: %v", err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write skill: %v", err)
	}
}
