package main

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
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

type providerBackend struct {
	provider string
	model    string
}

func (b providerBackend) Name() string {
	return "provider-test"
}

func (b providerBackend) Provider() string {
	return b.provider
}

func (b providerBackend) Model() string {
	return b.model
}

func (b providerBackend) ContextLimit() int {
	return 0
}

func (b providerBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{}
}

func (b providerBackend) Session() session.AgentSession {
	return nil
}

func (b providerBackend) SetStore(storage.Store) {}

func (b providerBackend) SetSession(storage.Session) {}

func (b providerBackend) SetConfig(*config.Config) {}

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

func TestCloseRuntimeOpenErrorClosesPartialHandles(t *testing.T) {
	agent := &printSession{}
	storageSession := &closeStorageSession{id: "partial"}

	err := closeRuntimeOpenError(
		"backend initialization error",
		context.Canceled,
		agent,
		storageSession,
	)
	if err == nil {
		t.Fatal("closeRuntimeOpenError returned nil")
	}
	if agent.closed != 1 {
		t.Fatalf("agent closed = %d, want 1", agent.closed)
	}
	if storageSession.closed != 1 {
		t.Fatalf("storage closed = %d, want 1", storageSession.closed)
	}
	if got := err.Error(); got != "backend initialization error: context canceled" {
		t.Fatalf("error = %q, want labeled context cancellation", got)
	}
}

func TestStartupProviderMissing(t *testing.T) {
	if !startupProviderMissing(providerBackend{}) {
		t.Fatal("empty provider should need startup setup")
	}
	if startupProviderMissing(providerBackend{provider: "openai"}) {
		t.Fatal("configured provider should not need startup setup")
	}
	if startupProviderMissing(nil) {
		t.Fatal("nil backend should not need startup setup")
	}
}

func TestStartupModelMissing(t *testing.T) {
	if startupModelMissing(providerBackend{}) {
		t.Fatal("empty provider should not need model setup")
	}
	if !startupModelMissing(providerBackend{provider: "openrouter"}) {
		t.Fatal("configured provider without model should need model setup")
	}
	if startupModelMissing(providerBackend{provider: "openrouter", model: "model-a"}) {
		t.Fatal("configured provider and model should not need model setup")
	}
	if startupModelMissing(nil) {
		t.Fatal("nil backend should not need model setup")
	}
}
