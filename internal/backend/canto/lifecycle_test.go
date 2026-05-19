package canto

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/storage"
)

func TestResumeLoadsRequestedStorageSession(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	seed, err := store.OpenSession(ctx, "/tmp/ion-resume-load", "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open seed session: %v", err)
	}
	seedID := seed.ID()
	if err := seed.Close(); err != nil {
		t.Fatalf("close seed session: %v", err)
	}

	provider := ctesting.NewFauxProvider("local-api", ctesting.Step{
		Chunks: []llm.Chunk{{Content: "resumed ok"}},
	})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})

	if err := b.Session().Resume(ctx, seedID); err != nil {
		t.Fatalf("resume backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if got := b.Session().ID(); got != seedID {
		t.Fatalf("session ID = %q, want %q", got, seedID)
	}

	if err := b.Session().SubmitTurn(ctx, "continue"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	if !requestHasMessage(calls[0].Messages, llm.RoleUser, "continue") {
		t.Fatalf("provider request missing resumed prompt: %#v", calls[0].Messages)
	}
}
