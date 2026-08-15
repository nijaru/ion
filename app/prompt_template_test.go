package app

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestPromptTemplateExpansionOnSubmit(t *testing.T) {
	tempDir := t.TempDir()
	promptsDir := filepath.Join(tempDir, ".ion", "prompts")
	if err := os.MkdirAll(promptsDir, 0755); err != nil {
		t.Fatal(err)
	}

	promptFile := filepath.Join(promptsDir, "review.md")
	content := `---
description: Review file
argument-hint: [file]
---
Please review $1 with focus on concurrency and data races.`

	if err := os.WriteFile(promptFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	model := readyModel(t)
	model.App.Workdir = tempDir

	expanded, ok := model.expandPromptTemplate("/review main.go")
	if !ok {
		t.Fatal("expected expandPromptTemplate to return true")
	}
	want := "Please review main.go with focus on concurrency and data races."
	if expanded != want {
		t.Fatalf("expanded = %q, want %q", expanded, want)
	}

	// Non-template slash commands should not expand
	_, ok = model.expandPromptTemplate("/help")
	if ok {
		t.Fatal("expected /help not to expand as a prompt template")
	}
}

func TestPromptTemplateAutocomplete(t *testing.T) {
	tempDir := t.TempDir()
	promptsDir := filepath.Join(tempDir, ".ion", "prompts")
	if err := os.MkdirAll(promptsDir, 0755); err != nil {
		t.Fatal(err)
	}

	promptFile := filepath.Join(promptsDir, "fix-bug.md")
	content := `---
description: Fix a reported bug
argument-hint: [issue-description]
---
Investigate and fix: $@`

	if err := os.WriteFile(promptFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	items := slashComposerCompletionItems("/fix", tempDir)
	if len(items) == 0 {
		t.Fatal("expected completion items for /fix")
	}

	found := false
	for _, it := range items {
		if it.Label == "/fix-bug" || strings.Contains(it.Label, "fix-bug") {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("did not find /fix-bug in completions: %v", items)
	}
}
