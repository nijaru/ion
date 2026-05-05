package skills

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestPreviewInstallValidatesSkillWithoutInstalling(t *testing.T) {
	source := writeInstallSkill(t, t.TempDir(), "review", "Review things.")
	targetRoot := t.TempDir()

	preview, err := PreviewInstall(source, targetRoot)
	if err != nil {
		t.Fatalf("PreviewInstall returned error: %v", err)
	}
	if preview.Name != "review" || preview.Description != "Review things." {
		t.Fatalf("preview = %#v, want review summary", preview)
	}
	if preview.Target != filepath.Join(targetRoot, "review") {
		t.Fatalf("target = %q, want target root/review", preview.Target)
	}
	if preview.Files != 2 || preview.Bytes == 0 {
		t.Fatalf(
			"preview files/bytes = %d/%d, want copied bundle stats",
			preview.Files,
			preview.Bytes,
		)
	}
	if _, err := os.Stat(preview.Target); !os.IsNotExist(err) {
		t.Fatalf("preview target stat = %v, want not installed", err)
	}
}

func TestInstallCopiesValidatedSkillBundle(t *testing.T) {
	source := writeInstallSkill(t, t.TempDir(), "review", "Review things.")
	scriptPath := filepath.Join(source, "resources", "run.sh")
	if err := os.WriteFile(scriptPath, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatalf("write executable resource: %v", err)
	}
	privateDir := filepath.Join(source, "private")
	if err := os.MkdirAll(privateDir, 0o700); err != nil {
		t.Fatalf("mkdir private dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(privateDir, "note.txt"), []byte("private"), 0o600); err != nil {
		t.Fatalf("write private resource: %v", err)
	}
	targetRoot := t.TempDir()

	preview, err := Install(source, targetRoot)
	if err != nil {
		t.Fatalf("Install returned error: %v", err)
	}
	detail, err := Read([]string{targetRoot}, preview.Name)
	if err != nil {
		t.Fatalf("Read installed skill: %v", err)
	}
	if detail.Name != "review" || !strings.Contains(detail.Instructions, "Use carefully.") {
		t.Fatalf("installed detail = %#v, want review instructions", detail)
	}
	if _, err := os.Stat(filepath.Join(targetRoot, ".staging")); err != nil {
		t.Fatalf("staging dir stat: %v", err)
	}
	assertMode(t, filepath.Join(targetRoot, "review", "resources", "run.sh"), 0o755)
	assertMode(t, filepath.Join(targetRoot, "review", "private"), 0o700)
	assertMode(t, filepath.Join(targetRoot, "review", "private", "note.txt"), 0o600)
}

func TestInstallRejectsExistingSkill(t *testing.T) {
	source := writeInstallSkill(t, t.TempDir(), "review", "Review things.")
	targetRoot := t.TempDir()
	if _, err := Install(source, targetRoot); err != nil {
		t.Fatalf("first Install returned error: %v", err)
	}
	_, err := Install(source, targetRoot)
	if err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Fatalf("second Install error = %v, want already exists", err)
	}
}

func TestInstallRejectsSymlinks(t *testing.T) {
	root := t.TempDir()
	source := writeInstallSkill(t, root, "review", "Review things.")
	if err := os.Symlink(filepath.Join(source, "SKILL.md"), filepath.Join(source, "link.md")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	_, err := PreviewInstall(source, t.TempDir())
	if err == nil || !strings.Contains(err.Error(), "unsupported non-regular file") {
		t.Fatalf("PreviewInstall error = %v, want symlink rejection", err)
	}
}

func writeInstallSkill(t *testing.T, root, name, description string) string {
	t.Helper()
	dir := filepath.Join(root, "source")
	if err := os.MkdirAll(filepath.Join(dir, "resources"), 0o755); err != nil {
		t.Fatalf("mkdir source: %v", err)
	}
	content := `---
name: ` + name + `
description: ` + description + `
allowed-tools: [read]
---
Use carefully.
`
	if err := os.WriteFile(filepath.Join(dir, "SKILL.md"), []byte(content), 0o644); err != nil {
		t.Fatalf("write skill: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "resources", "notes.txt"), []byte("notes"), 0o644); err != nil {
		t.Fatalf("write resource: %v", err)
	}
	return dir
}

func assertMode(t *testing.T, path string, want os.FileMode) {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat %s: %v", path, err)
	}
	if got := info.Mode().Perm(); got != want {
		t.Fatalf("mode %s = %v, want %v", path, got, want)
	}
}
