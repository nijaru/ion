package app

import (
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestMultilineHistoryNavigation(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"multiline\nhistory\nitem", "second item"}

	// 1. Enter history from draft
	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "second item" {
		t.Fatalf("expected 'second item', got %q", got)
	}

	// 2. Go to multiline item
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "multiline\nhistory\nitem" {
		t.Fatalf("expected multiline item, got %q", got)
	}
	
	// Cursor usually lands at the end after SetValue
	if got := model.Input.Composer.Line(); got != 2 {
		t.Fatalf("expected cursor at line 2, got %d", got)
	}

	// 3. Press Up - should move to line 1, NOT navigate history
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = updated.(Model)

	if model.Input.HistoryIdx != 0 {
		t.Fatalf("should still be at history index 0, got %d", model.Input.HistoryIdx)
	}
	if got := model.Input.Composer.Line(); got != 1 {
		t.Fatalf("expected cursor at line 1, got %d", got)
	}

	// 4. Press Up again - should move to line 0
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = updated.(Model)

	if got := model.Input.Composer.Line(); got != 0 {
		t.Fatalf("expected cursor at line 0, got %d", got)
	}

	// 5. Press Up at top - should NOT move because it's the first history item
	// (Actually it should try to move but history index is 0)
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	model = updated.(Model)
	if model.Input.HistoryIdx != 0 {
		t.Fatalf("expected still at history index 0, got %d", model.Input.HistoryIdx)
	}

	// 6. Press Down - should move to line 1
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = updated.(Model)

	if got := model.Input.Composer.Line(); got != 1 {
		t.Fatalf("expected cursor at line 1, got %d", got)
	}

	// 7. Press Down again - should move to line 2
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = updated.(Model)

	if got := model.Input.Composer.Line(); got != 2 {
		t.Fatalf("expected cursor at line 2, got %d", got)
	}

	// 8. Press Down at bottom - should move to next history item
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = updated.(Model)

	if model.Input.HistoryIdx != 1 {
		t.Fatalf("expected second history item (idx 1), but HistoryIdx is %d", model.Input.HistoryIdx)
	}
	if got := model.Input.Composer.Value(); got != "second item" {
		t.Fatalf("expected 'second item', got %q", got)
	}

	// 9. Press Down at bottom of second item - should exit history to draft
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	model = updated.(Model)

	if model.Input.HistoryIdx != -1 {
		t.Fatalf("expected exit to draft, but HistoryIdx is %d", model.Input.HistoryIdx)
	}
}

func TestCtrlPHistoryRespectsBoundaries(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"multiline\nitem"}

	// 1. Move to multiline item
	updated, _ := model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "multiline\nitem" {
		t.Fatalf("expected multiline item, got %q", got)
	}
	if got := model.Input.Composer.Line(); got != 1 {
		t.Fatalf("expected cursor at line 1, got %d", got)
	}

	// 2. Press Ctrl+P - should move cursor to line 0
	updated, _ = model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	model = updated.(Model)

	if got := model.Input.Composer.Line(); got != 0 {
		t.Fatalf("expected cursor at line 0, got %d", got)
	}
	if model.Input.HistoryIdx != 0 {
		t.Fatal("should not have navigated history yet")
	}

	// 3. Press Ctrl+P again - should try to navigate (but only 1 item)
	updated, _ = model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	model = updated.(Model)
	if model.Input.HistoryIdx != 0 {
		t.Fatal("should still be at index 0")
	}

	// 4. Press Ctrl+N - should move to line 1
	updated, _ = model.Update(tea.KeyPressMsg{Code: 'n', Mod: tea.ModCtrl})
	model = updated.(Model)

	if got := model.Input.Composer.Line(); got != 1 {
		t.Fatalf("expected cursor at line 1, got %d", got)
	}
	if model.Input.HistoryIdx != 0 {
		t.Fatal("should still be at index 0")
	}

	// 5. Press Ctrl+N again - should exit to draft
	updated, _ = model.Update(tea.KeyPressMsg{Code: 'n', Mod: tea.ModCtrl})
	model = updated.(Model)

	if model.Input.HistoryIdx != -1 {
		t.Fatal("expected exit to draft")
	}
}
