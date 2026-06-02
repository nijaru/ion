package main

import (
	"path/filepath"
	"testing"

	"github.com/nijaru/ion/session"
)

func TestSmokeBackendPersistsNativeTranscriptForResume(t *testing.T) {
	ctx := t.Context()
	root := t.TempDir()
	store, err := session.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	defer store.Close()

	opener, ok := store.(storeWithSessionID)
	if !ok {
		t.Fatal("store does not support deterministic session ids")
	}
	stored, err := opener.OpenSessionWithID(
		ctx,
		"smoke-persist-session",
		t.TempDir(),
		"fake/fake-model",
		"smoke",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	eventStore, err := session.NewSQLiteStore(filepath.Join(root, "sessions.db"))
	if err != nil {
		t.Fatalf("new event store: %v", err)
	}
	defer eventStore.Close()

	backend := newSmokeBackend("complete")
	backend.SetSession(stored)
	backend.SetCantoEventStore(eventStore)

	for _, event := range []session.AgentEvent{
		session.UserMessageEvent{Message: "build deterministic resume transcript"},
		session.TurnStartedEvent{},
		session.ToolCallStartedEvent{
			ToolUseID: "tool-1",
			ToolName:  "bash",
			Args:      `{"command":"sleep 2; echo ion-tmux-smoke"}`,
		},
		session.ToolResultEvent{
			ToolUseID: "tool-1",
			ToolName:  "bash",
			Result:    "ion-tmux-smoke\n",
		},
		session.AgentMessageEvent{Message: "done"},
		session.TurnFinishedEvent{},
	} {
		if !backend.emit(ctx, event) {
			t.Fatalf("emit failed for %T", event)
		}
	}

	entries, err := stored.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("entries length = %d, want 3: %#v", len(entries), entries)
	}
	if entries[0].Role != session.RoleUser ||
		entries[0].Content != "build deterministic resume transcript" {
		t.Fatalf("user entry = %#v", entries[0])
	}
	if entries[1].Role != session.RoleTool ||
		entries[1].Title != "Bash(sleep 2; echo ion-tmux-smoke)" ||
		entries[1].Content != "ion-tmux-smoke\n" {
		t.Fatalf("tool entry = %#v", entries[1])
	}
	if entries[2].Role != session.RoleAgent || entries[2].Content != "done" {
		t.Fatalf("agent entry = %#v", entries[2])
	}
}
