package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadPromptTemplatesFromDirsGlobalPrecedence(t *testing.T) {
	global := t.TempDir()
	project := t.TempDir()
	writeTemplate := func(dir, name, content string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(dir, name+".md"), []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	writeTemplate(global, "shared", "global")
	writeTemplate(project, "shared", "project")
	writeTemplate(project, "local", "project-only")
	if err := os.WriteFile(filepath.Join(project, "ignored.txt"), []byte("ignored"), 0o600); err != nil {
		t.Fatal(err)
	}

	got := loadPromptTemplatesFromDirs([]string{global, project})
	if got["shared"] != "global" || got["local"] != "project-only" {
		t.Fatalf("templates = %#v", got)
	}
	if _, ok := got["ignored"]; ok {
		t.Fatal("non-markdown file was loaded")
	}
}

func TestLoadPromptTemplatesIncludesProjectDirectory(t *testing.T) {
	home := t.TempDir()
	project := t.TempDir()
	t.Setenv("HOME", home)
	globalDir := filepath.Join(home, ".ion", "prompts")
	if err := os.MkdirAll(globalDir, 0o700); err != nil {
		t.Fatal(err)
	}
	projectDir := filepath.Join(project, ".ion", "prompts")
	if err := os.MkdirAll(projectDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(projectDir, "project.md"), []byte("project"), 0o600); err != nil {
		t.Fatal(err)
	}
	got := loadPromptTemplates(project)
	if got["project"] != "project" {
		t.Fatalf("templates = %#v", got)
	}
}

func TestLoadPromptTemplatesFromDirsMissingDirectoriesAreNonFatal(t *testing.T) {
	got := loadPromptTemplatesFromDirs([]string{filepath.Join(t.TempDir(), "missing")})
	if got != nil {
		t.Fatalf("templates = %#v, want nil", got)
	}
}
