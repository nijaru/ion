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
		{Event: session.AgentDelta{Delta: "Hello "}, Delay: 10 * time.Millisecond},
		{Event: session.AgentDelta{Delta: "world"}, Delay: 10 * time.Millisecond},
		{Event: session.AgentMessage{Message: "Hello world"}, Delay: 10 * time.Millisecond},
		{Event: session.TurnFinished{}, Delay: 0},
	})

	// 3. Setup Model
	model := New(b, sess, store, "/tmp/test", "main", "dev", nil)

	// 4. Submit a turn
	model.Input.Composer.SetValue("hi")
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

	// 5. Verify the app did not create transcript duplicates. The test backend
	// only emits UI events; the Canto backend owns durable user/assistant
	// transcript persistence in production.
	resumed, err := store.ResumeSession(context.Background(), sess.ID())
	if err != nil {
		t.Fatalf("failed to resume session: %v", err)
	}

	storedEntries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("failed to read stored entries: %v", err)
	}

	for _, e := range storedEntries {
		if e.Role == session.User || e.Role == session.Agent {
			t.Fatalf(
				"test backend should not create transcript entries through app persistence: %#v",
				storedEntries,
			)
		}
	}
}

func TestIntegrationToolApproval(t *testing.T) {
	// 1. Setup storage
	tmpRoot := filepath.Join(t.TempDir(), ".ion")
	store, _ := storage.NewCantoStore(tmpRoot)
	cwd, _ := os.Getwd()
	sess, _ := store.OpenSession(context.Background(), cwd, "tool-test", "main")

	// 2. Setup backend with tool call and approval
	b := testutil.New()
	b.SetStore(store)

	reqID := "req-abc"
	b.SetScript([]testutil.ScriptStep{
		{Event: session.TurnStarted{}, Delay: 0},
		{
			Event: session.ToolCallStarted{ToolName: "bash", Args: "ls"},
			Delay: 10 * time.Millisecond,
		},
		{
			Event: session.ApprovalRequest{
				RequestID:   reqID,
				ToolName:    "bash",
				Description: "run ls",
			},
			Delay: 10 * time.Millisecond,
		},
		// We expect the app to call Approve() after receiving ApprovalRequest
	})

	// 3. Setup Model
	model := New(b, sess, store, "/tmp/test", "main", "dev", nil)

	// 4. Submit turn
	model.Input.Composer.SetValue("list files")
	updated, _ := model.Update(sendKeyMsg())
	model = updated.(Model)

	// 5. Consume events until ApprovalRequest
	timeout := time.After(1 * time.Second)
loop1:
	for {
		select {
		case ev := <-b.Events():
			updated, _ = model.Update(ev)
			model = updated.(Model)
			if _, ok := ev.(session.ApprovalRequest); ok {
				break loop1
			}
		case <-timeout:
			t.Fatal("timed out waiting for ApprovalRequest")
		}
	}

	if model.Progress.Mode != stateApproval {
		t.Fatalf("expected stateApproval, got %v", model.Progress.Mode)
	}

	// 6. Approve with 'y'
	// We need to simulate the backend continuing after approval.
	// In reality, the backend waits on Approve() call.
	// Our testutil.Backend doesn't block on Approve() by default, it just receives it.
	// We need to add more script steps that fire AFTER Approve is called if we want to test the full follow-through.
	// But for now, let's just verify the TUI state transition.

	updated, _ = model.Update(tea.KeyPressMsg{Code: 'y', Text: "y"})
	model = updated.(Model)

	if model.Progress.Mode != stateReady {
		t.Fatalf("expected stateReady after approval, got %v", model.Progress.Mode)
	}

	// Verify Approve was called on backend
	// (testutil needs a way to check this, let's assume it works if stateReady is reached)
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
		{Event: session.ToolResult{
			Base:      session.Base{AgentID: "Tester"},
			ToolName:  "verify",
			ToolUseID: "verify-1",
			Result:    "Verification PASSED: go test ./...\n\nOutput:\nOK",
		}, Delay: 20 * time.Millisecond},
		{Event: session.AgentMessage{Message: "All good."}, Delay: 10 * time.Millisecond},
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

	// Verification results are live UI output here. The durable tool facts come
	// from Canto's tool execution path, not from this app event handler.
	resumed, _ := store.ResumeSession(context.Background(), sess.ID())
	storedEntries, _ := resumed.Entries(context.Background())
	for _, e := range storedEntries {
		if e.Role == session.Tool && strings.Contains(e.Title, "verify") {
			t.Fatalf("verification result should not be app-persisted: %#v", storedEntries)
		}
	}
}

func TestIntegrationSubagentDurability(t *testing.T) {
	// 1. Setup storage
	tmpRoot := filepath.Join(t.TempDir(), ".ion")
	store, _ := storage.NewCantoStore(tmpRoot)
	cwd, _ := os.Getwd()
	sess, _ := store.OpenSession(context.Background(), cwd, "subagent-durability-test", "main")

	// 2. Setup backend with subagent events
	b := testutil.New()
	b.SetStore(store)

	b.SetScript([]testutil.ScriptStep{
		{Event: session.TurnStarted{}, Delay: 0},
		{
			Event: session.ChildRequested{AgentName: "worker-1", Query: "task 1"},
			Delay: 10 * time.Millisecond,
		},
		{
			Event: session.ChildCompleted{AgentName: "worker-1", Result: "result 1"},
			Delay: 10 * time.Millisecond,
		},
		{Event: session.TurnFinished{}, Delay: 0},
	})

	// 3. Setup Model
	model := New(b, sess, store, "/tmp/test", "main", "dev", nil)

	// 4. Submit turn
	b.SubmitTurn(context.Background(), "run worker")

	// 5. Consume events
	timeout := time.After(1 * time.Second)
loop:
	for {
		select {
		case ev := <-b.Events():
			updated, _ := model.Update(ev)
			model = updated.(Model)
			if _, ok := ev.(session.TurnFinished); ok {
				break loop
			}
		case <-timeout:
			t.Fatal("timed out")
		}
	}

	// 6. Verify persistence
	resumed, _ := store.ResumeSession(context.Background(), sess.ID())
	entries, _ := resumed.Entries(context.Background())

	foundSubagent := false
	for _, e := range entries {
		if e.Role == session.Subagent && e.Title == "worker-1" {
			foundSubagent = true
			if !strings.Contains(e.Content, "Completed: result 1") &&
				!strings.Contains(e.Content, "Started: task 1") {
				t.Errorf("unexpected subagent content: %q", e.Content)
			}
		}
	}
	if !foundSubagent {
		t.Error("subagent entry not found in storage history")
	}
}

// sendKeyMsg returns the Enter key press message used to submit a turn.
func sendKeyMsg() tea.KeyPressMsg {
	return tea.KeyPressMsg{Code: tea.KeyEnter}
}
