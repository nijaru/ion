package skills

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestNoticeListsInstalledSkills(t *testing.T) {
	root := t.TempDir()
	writeSkill(t, filepath.Join(root, "go-review", "SKILL.md"), `---
name: go-review
description: Review Go changes before commit.
allowed-tools: [read, grep]
---
Use focused review findings.
`)

	notice, err := Notice([]string{root}, "")
	if err != nil {
		t.Fatalf("Notice returned error: %v", err)
	}
	for _, want := range []string{
		"skills",
		root,
		"go-review: Review Go changes before commit.",
		"tools: read, grep",
	} {
		if !strings.Contains(notice, want) {
			t.Fatalf("notice missing %q:\n%s", want, notice)
		}
	}
}

func TestNoticeFiltersByQuery(t *testing.T) {
	root := t.TempDir()
	writeSkill(t, filepath.Join(root, "go-review", "SKILL.md"), `---
name: go-review
description: Review Go changes before commit.
---
Use focused review findings.
`)
	writeSkill(t, filepath.Join(root, "python-style", "SKILL.md"), `---
name: python-style
description: Keep Python code idiomatic.
---
Use uv and ruff.
`)

	notice, err := Notice([]string{root}, "python")
	if err != nil {
		t.Fatalf("Notice returned error: %v", err)
	}
	if !strings.Contains(notice, "python-style") {
		t.Fatalf("notice missing matching skill:\n%s", notice)
	}
	if strings.Contains(notice, "go-review") {
		t.Fatalf("notice included non-matching skill:\n%s", notice)
	}
}

func TestNoticeHandlesMissingDirectory(t *testing.T) {
	root := filepath.Join(t.TempDir(), "missing")
	notice, err := Notice([]string{root}, "")
	if err != nil {
		t.Fatalf("Notice returned error: %v", err)
	}
	if !strings.Contains(notice, "No installed skills found.") {
		t.Fatalf("notice = %q, want empty message", notice)
	}
}

func writeSkill(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir skill dir: %v", err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write skill: %v", err)
	}
}
