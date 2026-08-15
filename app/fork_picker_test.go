package app

import (
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

func TestUserMessageForkPickerNavigationAndSelection(t *testing.T) {
	model := readyModel(t)
	now := time.Now()

	msg1 := session.NewUserText("build a cli tool", now)
	e1 := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "msg-1", ParentID: "root", Timestamp: now},
		Message:   msg1,
	}
	msg2 := session.NewUserText("add unit tests", now.Add(time.Minute))
	e2 := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "msg-2", ParentID: "msg-1", Timestamp: now.Add(time.Minute)},
		Message:   msg2,
	}

	runner := &stubRunner{
		branchEntries: []session.Entry{e1, e2},
	}
	model.Model.Runner = runner

	// 1. Open fork picker
	updated, cmd := model.openUserMessageForkPicker()
	if cmd == nil {
		t.Fatal("openUserMessageForkPicker returned nil command")
	}
	if updated.Picker.UserMessage == nil || !updated.Picker.UserMessage.loading {
		t.Fatal("UserMessage picker not in loading state")
	}

	// 2. Resolve loaded messages
	loadMsg := cmd().(userMessagesLoadedMsg)
	if len(loadMsg.items) != 2 {
		t.Fatalf("loaded %d items, want 2", len(loadMsg.items))
	}
	updated, _ = updated.handleUserMessagesLoaded(loadMsg)
	if updated.Picker.UserMessage.selectedIndex != 1 {
		t.Fatalf("default selectedIndex = %d, want 1 (most recent)", updated.Picker.UserMessage.selectedIndex)
	}

	// 3. Move Up to previous message
	updated, _ = updated.handleUserMessageForkPickerKey(tea.KeyPressMsg{Code: tea.KeyUp})
	if updated.Picker.UserMessage.selectedIndex != 0 {
		t.Fatalf("selectedIndex after Up = %d, want 0", updated.Picker.UserMessage.selectedIndex)
	}

	// 4. Move Up again (wrap around to bottom)
	updated, _ = updated.handleUserMessageForkPickerKey(tea.KeyPressMsg{Code: tea.KeyUp})
	if updated.Picker.UserMessage.selectedIndex != 1 {
		t.Fatalf("selectedIndex after wrap Up = %d, want 1", updated.Picker.UserMessage.selectedIndex)
	}

	// 5. Select message 1 ("build a cli tool")
	updated, _ = updated.handleUserMessageForkPickerKey(tea.KeyPressMsg{Code: tea.KeyUp}) // back to 0
	next, selectCmd := updated.handleUserMessageForkPickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	if selectCmd == nil {
		t.Fatal("Enter returned nil command")
	}
	if next.Picker.UserMessage != nil {
		t.Fatal("UserMessage picker still open after Enter")
	}

	undoMsg := selectCmd().(undoResultMsg)
	if undoMsg.targetLeafID != "root" {
		t.Fatalf("undo targetLeafID = %q, want 'root'", undoMsg.targetLeafID)
	}
	if undoMsg.promptText != "build a cli tool" {
		t.Fatalf("undo promptText = %q, want 'build a cli tool'", undoMsg.promptText)
	}
}

func TestDoubleEscapeTriggersUserMessageForkPicker(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{DoubleEscapeAction: "fork"}
	now := time.Now()

	msg1 := session.NewUserText("initial prompt", now)
	e1 := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "msg-1", ParentID: "root", Timestamp: now},
		Message:   msg1,
	}
	runner := &stubRunner{
		branchEntries: []session.Entry{e1},
	}
	model.Model.Runner = runner

	// First escape records LastEscAt
	updated, cmd := model.handleKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if cmd != nil {
		t.Fatal("first escape returned command")
	}
	if updated.Picker.LastEscAt.IsZero() {
		t.Fatal("first escape did not record LastEscAt")
	}

	// Second escape within 500ms triggers openUserMessageForkPicker
	next, cmd2 := updated.handleKey(tea.KeyPressMsg{Code: tea.KeyEscape})
	if cmd2 == nil {
		t.Fatal("second escape returned nil command")
	}
	if next.Picker.UserMessage == nil || !next.Picker.UserMessage.loading {
		t.Fatal("second escape did not open UserMessage picker")
	}
}
