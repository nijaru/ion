package app

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/testutil"
)

func TestIntegrationFullLoop(t *testing.T) {
	// 1. Setup storage
	tmpRoot := filepath.Join(t.TempDir(), ".ion")
	store, err := storage.NewCantoStore(tmpRoot)
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
		{Event: session.TurnStarted{}, Delay: 0},
		{Event: session.AssistantDelta{Delta: "Hello "}, Delay: 10 * time.Millisecond},
		{Event: session.AssistantDelta{Delta: "world"}, Delay: 10 * time.Millisecond},
		{Event: session.AssistantMessage{Message: "Hello world"}, Delay: 10 * time.Millisecond},
		{Event: session.TurnFinished{}, Delay: 0},
	})

	// 3. Setup Model
	model := New(b, sess, store, "/tmp/test", "main", "dev", nil)

	// 4. Submit a turn
	model.composer.SetValue("hi")
	// simulate ctrl+s (m.sendKey)
	updated, _ := model.Update(sendKeyMsg())
	model = updated.(Model)

	// Wait for async backend script to finish
	timeout := time.After(500 * time.Millisecond)
	done := false
	for !done {
		select {
		case ev := <-b.Events():
			updated, _ = model.Update(ev)
			model = updated.(Model)
			if _, ok := ev.(session.TurnFinished); ok {
				done = true
			}
		case <-timeout:
			t.Fatalf("timed out waiting for turn to finish")
		}
	}

	// 5. Verify storage has the entries
	resumed, err := store.ResumeSession(context.Background(), sess.ID())
	if err != nil {
		t.Fatalf("failed to resume session: %v", err)
	}

	storedEntries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("failed to read stored entries: %v", err)
	}

	foundUser := false
	foundAsst := false
	for _, e := range storedEntries {
		if e.Role == session.User && e.Content == "hi" {
			foundUser = true
		}
		if e.Role == session.Assistant && e.Content == "Hello world" {
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

func TestMultiplexedSwarms(t *testing.T) {
	// Setup store and session
	tmpRoot := filepath.Join(t.TempDir(), ".ion")
	store, _ := storage.NewCantoStore(tmpRoot)
	cwd, _ := os.Getwd()
	sess, _ := store.OpenSession(context.Background(), cwd, "swarm-test", "main")

	// Setup script with two sub-agents
	b := testutil.New()
	b.SetScript([]testutil.ScriptStep{
		{Event: session.TurnStarted{}, Delay: 0},
		{
			Event: session.StatusChanged{
				Base:   session.Base{AgentID: "Explorer"},
				Status: "Mapping codebase...",
			},
			Delay: 10 * time.Millisecond,
		},
		{Event: session.ToolCallStarted{
			Base:     session.Base{AgentID: "Tester"},
			ToolName: "verify",
			Args:     "go test ./...",
		}, Delay: 10 * time.Millisecond},
		{Event: session.VerificationResult{
			Base:    session.Base{AgentID: "Tester"},
			Command: "go test ./...",
			Passed:  true,
			Metric:  "15/15 passed",
			Output:  "OK",
		}, Delay: 20 * time.Millisecond},
		{Event: session.AssistantMessage{Message: "All good."}, Delay: 10 * time.Millisecond},
		{Event: session.TurnFinished{}, Delay: 0},
	})

	model := New(b, sess, store, "/tmp/test", "main", "dev", nil)

	// Manually trigger turn
	b.SubmitTurn(context.Background(), "status check")

	// Consume events
	timeout := time.After(500 * time.Millisecond)
	done := false
	for !done {
		select {
		case ev := <-b.Events():
			updated, _ := model.Update(ev)
			model = updated.(Model)
			if _, ok := ev.(session.TurnFinished); ok {
				done = true
			}
		case <-timeout:
			t.Fatalf("timed out")
		}
	}

	// Verify entries from storage
	resumed, _ := store.ResumeSession(context.Background(), sess.ID())
	storedEntries, _ := resumed.Entries(context.Background())

	foundVerify := false
	for _, e := range storedEntries {
		if e.Role == session.Tool && strings.Contains(e.Title, "verify") {
			foundVerify = true
			if !strings.Contains(e.Content, "PASSED: 15/15 passed") {
				t.Errorf("unexpected verification content: %q", e.Content)
			}
		}
	}
	if !foundVerify {
		t.Error("verification result not found in storage")
	}
}

// sendKeyMsg returns the Enter key press message used to submit a turn.
func sendKeyMsg() tea.KeyPressMsg {
	return tea.KeyPressMsg{Code: tea.KeyEnter}
}
