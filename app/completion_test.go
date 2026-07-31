package app

import (
	"os"
	"path/filepath"
	"testing"
)

func TestStaleFileCompletionCannotApplyAfterRuntimeReplacement(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("@src")
	requestID := model.inputReducer().beginFileCompletionRequest()
	model.Model.EventGeneration++

	next, cmd := model.handleFileReferenceCompletion(fileReferenceCompletionMsg{
		generation: model.Model.EventGeneration - 1,
		requestID:  requestID,
		text:       "@src",
		token:      "@src",
		matches:    []fileReferenceMatch{{reference: "@src/main.go"}},
	})
	if cmd != nil {
		t.Fatal("stale file completion returned a command")
	}
	if next.Input.Completion != nil {
		t.Fatal("stale file completion changed the replacement runtime")
	}
}

func TestCustomComposerCompletionItems(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	// Write mock skills
	skillsDir := filepath.Join(home, ".ion", "skills")
	skills := []string{"review", "refactor"}
	for _, s := range skills {
		skillPath := filepath.Join(skillsDir, s, "SKILL.md")
		if err := os.MkdirAll(filepath.Dir(skillPath), 0o755); err != nil {
			t.Fatalf("mkdir skill dir: %v", err)
		}
		content := "---\nname: " + s + "\ndescription: Test skill.\n---\nBody"
		if err := os.WriteFile(skillPath, []byte(content), 0o644); err != nil {
			t.Fatalf("write skill file: %v", err)
		}
	}

	model := New(stubBackend{}, nil, nil, "/tmp/test", "main", "dev", nil)

	// Set composer draft
	model.Input.Composer.SetValue("//re")

	// Get completions. Skill discovery is asynchronous so it cannot block
	// Bubble Tea Update while walking the skill tree.
	_, cmd := model.composerCompletionItems()
	if cmd == nil {
		t.Fatal("expected asynchronous skill completion command")
	}
	message, ok := cmd().(skillCompletionMsg)
	if !ok {
		t.Fatalf("completion message = %T, want skillCompletionMsg", cmd())
	}
	model, _ = model.handleSkillCompletion(message)
	items := model.Input.Completion.items

	if len(items) != 2 {
		t.Fatalf("expected 2 completions, got %d", len(items))
	}

	labels := []string{items[0].Label, items[1].Label}
	if !((labels[0] == "//review" && labels[1] == "//refactor") || (labels[0] == "//refactor" && labels[1] == "//review")) {
		t.Fatalf("unexpected completion labels: %v", labels)
	}

	// Unique query
	model.Input.Composer.SetValue("//ref")
	_, cmd = model.composerCompletionItems()
	if cmd == nil {
		t.Fatal("expected asynchronous unique skill completion command")
	}
	message = cmd().(skillCompletionMsg)
	model, _ = model.handleSkillCompletion(message)
	items = model.Input.Completion.items
	if len(items) != 1 || items[0].Label != "//refactor" {
		t.Fatalf("expected unique refactor completion, got: %v", items)
	}
}

func TestCompleteCustomCommandRouting(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	// Write mock skills
	skillsDir := filepath.Join(home, ".ion", "skills")
	skills := []string{"review", "refactor"}
	for _, s := range skills {
		skillPath := filepath.Join(skillsDir, s, "SKILL.md")
		if err := os.MkdirAll(filepath.Dir(skillPath), 0o755); err != nil {
			t.Fatalf("mkdir skill dir: %v", err)
		}
		content := "---\nname: " + s + "\ndescription: Test skill.\n---\nBody"
		if err := os.WriteFile(skillPath, []byte(content), 0o644); err != nil {
			t.Fatalf("write skill file: %v", err)
		}
	}

	model := New(stubBackend{}, nil, nil, "/tmp/test", "main", "dev", nil)

	// Test case 1: Unique match complete inline (with a trailing space)
	model.Input.Composer.SetValue("//ref")
	model, cmd, ok := model.completeSlashCommand()
	if !ok {
		t.Fatal("expected completion to handle custom command")
	}
	if cmd == nil {
		t.Fatal("expected asynchronous custom command completion")
	}
	message, ok := cmd().(skillCompletionMsg)
	if !ok {
		t.Fatalf("completion message = %T, want skillCompletionMsg", cmd())
	}
	model, cmd = model.handleSkillCompletion(message)
	if model.Input.Composer.Value() != "//refactor " {
		t.Fatalf("expected completed composer value to be '//refactor ', got %q", model.Input.Composer.Value())
	}
	if cmd != nil {
		t.Fatalf("expected no follow-up command after unique completion, got %T", cmd)
	}

	// Test case 2: Ambiguous matches open command picker
	model.Input.Composer.SetValue("//re")
	model, cmd, ok = model.completeSlashCommand()
	if !ok {
		t.Fatal("expected completion to handle custom command")
	}
	if cmd == nil {
		t.Fatal("expected asynchronous custom command completion")
	}
	message, ok = cmd().(skillCompletionMsg)
	if !ok {
		t.Fatalf("completion message = %T, want skillCompletionMsg", cmd())
	}
	model, cmd = model.handleSkillCompletion(message)
	if cmd != nil {
		t.Fatal("expected picker opening to return no cmd")
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeCommand {
		t.Fatal("expected picker overlay to open for ambiguous custom commands")
	}
	if len(model.Picker.Overlay.items) != 2 {
		t.Fatalf("expected 2 items in custom command picker, got %d", len(model.Picker.Overlay.items))
	}
}
