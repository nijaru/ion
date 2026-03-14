package app

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/testutil"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestIntegrationFullLoop(t *testing.T) {
	// 1. Setup storage
	tmpRoot := filepath.Join(t.TempDir(), ".ion")
	store, err := storage.NewFileStore(tmpRoot)
	if err != nil {
		t.Fatalf("failed to create store: %v", err)
	}

	cwd, _ := os.Getwd()
	sess, err := store.OpenSession(context.Background(), cwd, "fake-model", "main")
	if err != nil {
		t.Fatalf("failed to open session: %v", err)
	}

	// 2. Setup backend with script
	b := testutil.New()
	b.SetStore(store)
	b.SetScript([]testutil.ScriptStep{
		{Event: session.EventTurnStarted{BaseEvent: session.BaseEvent{}}, Delay: 0},
		{Event: session.EventAssistantDelta{BaseEvent: session.BaseEvent{}, Delta: "Hello "}, Delay: 10 * time.Millisecond},
		{Event: session.EventAssistantDelta{BaseEvent: session.BaseEvent{}, Delta: "world"}, Delay: 10 * time.Millisecond},
		{Event: session.EventAssistantMessage{BaseEvent: session.BaseEvent{}, Message: "Hello world"}, Delay: 10 * time.Millisecond},
		{Event: session.EventTurnFinished{BaseEvent: session.BaseEvent{}}, Delay: 0},
	})

	// 3. Setup Model
	model := New(b, sess)
	
	// 4. Submit a turn
	model.composer.SetValue("hi")
	// simulate ctrl+s (m.sendKey)
	updated, _ := model.Update(model.sendKeyMsg())
	model = updated.(Model)

	// Wait for async backend script to finish
	// In a real TUI test we'd loop, here we just wait or manually trigger updates
	timeout := time.After(500 * time.Millisecond)
	done := false
	for !done {
		select {
		case ev := <-b.Events():
			updated, _ = model.Update(ev)
			model = updated.(Model)
			if _, ok := ev.(session.EventTurnFinished); ok {
				done = true
			}
		case <-timeout:
			t.Fatalf("timed out waiting for turn to finish")
		}
	}

	// 5. Verify transcript has 3 entries: boot assistant, user, and assistant
	// Wait, boot entries from Bootstrap() are prepended.
	if len(model.entries) < 2 {
		t.Fatalf("expected at least 2 entries, got %d", len(model.entries))
	}
	
	lastEntry := model.entries[len(model.entries)-1]
	if lastEntry.Content != "Hello world" {
		t.Fatalf("expected last entry to be 'Hello world', got %q", lastEntry.Content)
	}

	// 6. Verify storage has the entries
	sess.Close()
	resumed, err := store.ResumeSession(context.Background(), sess.ID())
	if err != nil {
		t.Fatalf("failed to resume session: %v", err)
	}
	
	storedEntries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("failed to read stored entries: %v", err)
	}

	// Storage should contain: meta (skipped), user "hi", assistant "Hello world"
	foundUser := false
	foundAsst := false
	for _, e := range storedEntries {
		if e.Role == session.RoleUser && e.Content == "hi" {
			foundUser = true
		}
		if e.Role == session.RoleAssistant && e.Content == "Hello world" {
			foundAsst = true
		}
	}

	if !foundUser {
		t.Errorf("user message 'hi' not found in storage")
	}
	if !foundAsst {
		t.Errorf("assistant message 'Hello world' not found in storage")
	}
}

// helper to simulate the send key message
func (m Model) sendKeyMsg() any {
	// In Bubble Tea v2, KeyPressMsg is a type alias for Key, which uses Text.
	return tea.KeyPressMsg{Text: m.sendKey}
}
