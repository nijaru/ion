package main

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type closeStorageSession struct {
	id     string
	closed int
}

func (s *closeStorageSession) ID() string { return s.id }

func (s *closeStorageSession) Meta() storage.Metadata {
	return storage.Metadata{ID: s.id, CreatedAt: time.Now()}
}

func (s *closeStorageSession) Append(ctx context.Context, event any) error { return nil }

func (s *closeStorageSession) Entries(ctx context.Context) ([]session.Entry, error) {
	return nil, nil
}

func (s *closeStorageSession) LastStatus(ctx context.Context) (string, error) { return "", nil }

func (s *closeStorageSession) Usage(ctx context.Context) (int, int, float64, error) {
	return 0, 0, 0, nil
}

func (s *closeStorageSession) Close() error {
	s.closed++
	return nil
}

func TestRuntimeHandlesForCloseUsesFinalAppRuntime(t *testing.T) {
	startupAgent := &printSession{}
	currentAgent := &printSession{}
	startupStorage := &closeStorageSession{id: "startup"}
	currentStorage := &closeStorageSession{id: "current"}
	final := app.Model{
		Model: app.ModelState{
			Session: currentAgent,
			Storage: currentStorage,
		},
	}

	agent, storageSession := runtimeHandlesForClose(final, startupAgent, startupStorage)
	if agent != currentAgent {
		t.Fatalf("agent = %#v, want current runtime agent", agent)
	}
	if storageSession != currentStorage {
		t.Fatalf("storage = %#v, want current runtime storage", storageSession)
	}
}

func TestRuntimeHandlesForCloseFallsBackForNonAppModel(t *testing.T) {
	startupAgent := &printSession{}
	startupStorage := &closeStorageSession{id: "startup"}

	agent, storageSession := runtimeHandlesForClose(nil, startupAgent, startupStorage)
	if agent != startupAgent {
		t.Fatalf("agent = %#v, want fallback agent", agent)
	}
	if storageSession != startupStorage {
		t.Fatalf("storage = %#v, want fallback storage", storageSession)
	}
}
