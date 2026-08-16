package app

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/prompts"
)

func TestPromptTemplateExpansionOnSubmit(t *testing.T) {
	model := readyModel(t).WithPromptTemplates([]prompts.PromptTemplate{{
		Name:         "review",
		Description:  "Review file",
		ArgumentHint: "[file]",
		Content:      "Please review $1 with focus on concurrency and data races.",
	}})

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
	items := slashComposerCompletionItems("/fix", []prompts.PromptTemplate{{
		Name:         "fix-bug",
		Description:  "Fix a reported bug",
		ArgumentHint: "[issue-description]",
		Content:      "Investigate and fix: $@",
	}})
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

func TestPromptTemplateExpansionDoesNotReadWorkspaceFiles(t *testing.T) {
	workdir := t.TempDir()
	promptDir := filepath.Join(workdir, ".ion", "prompts")
	if err := os.MkdirAll(promptDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(promptDir, "workspace-only.md"), []byte("untrusted"), 0o600); err != nil {
		t.Fatal(err)
	}

	model := readyModel(t)
	model.App.Workdir = workdir

	if expanded, ok := model.expandPromptTemplate("/workspace-only"); ok || expanded != "/workspace-only" {
		t.Fatalf("workspace prompt unexpectedly expanded: %q, %v", expanded, ok)
	}
}
