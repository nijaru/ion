package app

import (
	"context"
	"testing"

	"github.com/nijaru/ion/session"
)

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
