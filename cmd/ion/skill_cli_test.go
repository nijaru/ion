package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestSkillInstallPreviewDoesNotInstall(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	source := writeCLITestSkill(t, t.TempDir(), "review")

	var out bytes.Buffer
	if err := runSkillCommand([]string{"install", source}, &out); err != nil {
		t.Fatalf("runSkillCommand returned error: %v", err)
	}
	text := out.String()
	for _, want := range []string{
		"Skill install preview",
		"name: review",
		"run: ion skill install --confirm",
	} {
		if !strings.Contains(text, want) {
			t.Fatalf("preview output missing %q:\n%s", want, text)
		}
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "skills", "review")); !os.IsNotExist(err) {
		t.Fatalf("preview target stat = %v, want not installed", err)
	}
}

func TestSkillInstallConfirmInstalls(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	source := writeCLITestSkill(t, t.TempDir(), "review")

	var out bytes.Buffer
	if err := runSkillCommand([]string{"install", "--confirm", source}, &out); err != nil {
		t.Fatalf("runSkillCommand returned error: %v", err)
	}
	if !strings.Contains(out.String(), "Skill installed") {
		t.Fatalf("install output = %q, want installed", out.String())
	}
	if _, err := os.Stat(filepath.Join(home, ".ion", "skills", "review", "SKILL.md")); err != nil {
		t.Fatalf("installed SKILL.md stat: %v", err)
	}
}

func TestSkillListUsesInstalledSkills(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	source := writeCLITestSkill(t, t.TempDir(), "review")
	if err := runSkillCommand([]string{"install", "--confirm", source}, &bytes.Buffer{}); err != nil {
		t.Fatalf("install skill: %v", err)
	}

	var out bytes.Buffer
	if err := runSkillCommand([]string{"list", "rev"}, &out); err != nil {
		t.Fatalf("list skills: %v", err)
	}
	if !strings.Contains(out.String(), "review: Review changes.") {
		t.Fatalf("list output = %q, want review", out.String())
	}
}

func TestRunTopLevelCommandHandlesSkill(t *testing.T) {
	var stdout, stderr bytes.Buffer
	handled, code := runTopLevelCommand([]string{"skill"}, &stdout, &stderr)
	if !handled || code != 1 {
		t.Fatalf("handled/code = %v/%d, want handled failure", handled, code)
	}
	if !strings.Contains(stderr.String(), "ion skill install") {
		t.Fatalf("stderr = %q, want usage", stderr.String())
	}
}

func writeCLITestSkill(t *testing.T, root, name string) string {
	t.Helper()
	dir := filepath.Join(root, name)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("mkdir skill: %v", err)
	}
	content := `---
name: ` + name + `
description: Review changes.
allowed-tools: [read]
---
Inspect the diff.
`
	if err := os.WriteFile(filepath.Join(dir, "SKILL.md"), []byte(content), 0o644); err != nil {
		t.Fatalf("write skill: %v", err)
	}
	return dir
}
