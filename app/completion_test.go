package app

import (
	"os"
	"path/filepath"
	"testing"

	tea "charm.land/bubbletea/v2"
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

func TestCtrlROpensHistoryPickerAndSelectsItem(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"fix compiler errors", "run test suite", "add new feature"}

	// Press Ctrl+R
	next, cmd := model.Update(tea.KeyPressMsg{Code: 'r', Mod: tea.ModCtrl, Text: "ctrl+r"})
	model = testModel(t, next)
	if cmd != nil {
		runCommandTree(t, cmd)
	}

	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeHistory {
		t.Fatal("expected history picker overlay to open on ctrl+r")
	}
	if len(model.Picker.Overlay.items) != 3 {
		t.Fatalf("expected 3 items in history picker, got %d", len(model.Picker.Overlay.items))
	}
	// Verify most recent is first
	if model.Picker.Overlay.items[0].Value != "add new feature" {
		t.Fatalf("item 0 = %q, want %q", model.Picker.Overlay.items[0].Value, "add new feature")
	}

	// Press Enter to select the top item
	next, cmd = model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, next)
	if cmd != nil {
		runCommandTree(t, cmd)
	}

	if model.Picker.Overlay != nil {
		t.Fatal("expected picker overlay to close after selection")
	}
	if got := model.Input.Composer.Value(); got != "add new feature" {
		t.Fatalf("composer value = %q, want %q", got, "add new feature")
	}
}

func TestCompletionNavigationUpDownAndTab(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("check @")
	model.inputReducer().setCompletionItems([]completionItem{
		{Label: "@app/events.go", Detail: ""},
		{Label: "@app/model.go", Detail: ""},
		{Label: "@app/input.go", Detail: ""},
	})

	if model.Input.Completion.index != 0 {
		t.Fatalf("initial index = %d, want 0", model.Input.Completion.index)
	}

	// Press Down Arrow
	next, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = testModel(t, next)
	if model.Input.Completion.index != 1 {
		t.Fatalf("index after down = %d, want 1", model.Input.Completion.index)
	}

	// Press Down Arrow again
	next, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = testModel(t, next)
	if model.Input.Completion.index != 2 {
		t.Fatalf("index after down = %d, want 2", model.Input.Completion.index)
	}

	// Press Down Arrow again to wrap around
	next, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = testModel(t, next)
	if model.Input.Completion.index != 0 {
		t.Fatalf("index after down wrap = %d, want 0", model.Input.Completion.index)
	}

	// Press Up Arrow to wrap backward to last item
	next, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = testModel(t, next)
	if model.Input.Completion.index != 2 {
		t.Fatalf("index after up = %d, want 2", model.Input.Completion.index)
	}

	// Press Tab to insert the selected item (@app/input.go)
	next, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
	model = testModel(t, next)
	if model.Input.Completion != nil {
		t.Fatal("expected completion to close after tab insertion")
	}
	if got := model.Input.Composer.Value(); got != "check @app/input.go " {
		t.Fatalf("composer value = %q, want %q", got, "check @app/input.go ")
	}
}
