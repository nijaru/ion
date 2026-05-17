package canto

import (
	"context"
	"testing"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/storage"
)

func TestProviderAndModelLoadFromEnv(t *testing.T) {
	t.Setenv("ION_PROVIDER", "anthropic")
	t.Setenv("ION_MODEL", "claude-sonnet-4-5")

	b := New()

	if got := b.Provider(); got != "anthropic" {
		t.Fatalf("Provider() = %q, want %q", got, "anthropic")
	}
	if got := b.Model(); got != "claude-sonnet-4-5" {
		t.Fatalf("Model() = %q, want %q", got, "claude-sonnet-4-5")
	}
}

func TestSetConfigCopiesProviderAndModel(t *testing.T) {
	t.Setenv("ION_PROVIDER", "")
	t.Setenv("ION_MODEL", "")

	cfg := &config.Config{Provider: "openai", Model: "model-a"}
	b := New()
	b.SetConfig(cfg)

	cfg.Provider = "anthropic"
	cfg.Model = "model-b"

	if got := b.Provider(); got != "openai" {
		t.Fatalf("Provider() = %q, want copied openai", got)
	}
	if got := b.Model(); got != "model-a" {
		t.Fatalf("Model() = %q, want copied model-a", got)
	}

	b.SetConfig(nil)
	if got := b.Provider(); got != "" {
		t.Fatalf("Provider() after nil config = %q, want empty", got)
	}
	if got := b.Model(); got != "" {
		t.Fatalf("Model() after nil config = %q, want empty", got)
	}
}

func TestSubmitTurnPreservesProviderInSessionMetadata(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := "/tmp/ion-local-api"
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := llm.NewFauxProvider("local-api", llm.FauxStep{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("sessions = %d, want 1", len(sessions))
	}
	if sessions[0].Model != "local-api/model-a" {
		t.Fatalf("session model = %q, want provider-qualified model", sessions[0].Model)
	}
}
