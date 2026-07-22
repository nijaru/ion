package app

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/session"
)

func TestCurrentResumeLeafIDDoesNotUseStableSessionIdentity(t *testing.T) {
	model := readyModel(t)
	model.Model.Runtime = Snapshot{
		SessionID:    "stable-session-id",
		Materialized: true,
	}
	model.Model.LeafID = "conversation-leaf-id"

	if got := model.currentResumeLeafID(); got != "conversation-leaf-id" {
		t.Fatalf("resume leaf = %q, want conversation-leaf-id", got)
	}

	model.Model.LeafID = ""
	if got := model.currentResumeLeafID(); got != "" {
		t.Fatalf("resume leaf without a selected entry = %q, want empty", got)
	}
}

func TestDirectModelCommandRequiresIdle(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true

	_, cmd := model.handleCommand("/model model-b")
	if cmd == nil {
		t.Fatal("model command while a turn is active returned no guard")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "Finish or cancel the current turn") {
		t.Fatalf("error = %v, want busy-turn guard", err)
	}
}

type lookupSessionStore struct {
	resumeOnlyStore
	info session.SessionInfoEntry
}

func (s *lookupSessionStore) GetSessionInfo(context.Context, string) (session.SessionInfoEntry, error) {
	return s.info, nil
}

func TestStoredSessionConfigUsesDirectCatalogLookupForForeignWorkdir(t *testing.T) {
	model := readyModel(t)
	store := &lookupSessionStore{
		info: session.SessionInfoEntry{
			EntryBase: session.EntryBase{ID: "foreign-session"},
			Model:     "openai/gpt-4.1",
		},
	}

	cfg, err := model.storedSessionConfig(context.Background(), store, "foreign-session")
	if err != nil {
		t.Fatalf("storedSessionConfig() error = %v", err)
	}
	if cfg.Provider != "openai" || cfg.Model != "gpt-4.1" {
		t.Fatalf("stored session config = %s/%s, want openai/gpt-4.1", cfg.Provider, cfg.Model)
	}
}
