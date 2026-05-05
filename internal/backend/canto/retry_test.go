package canto

import (
	"context"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestOpenRetriesTransientProviderErrors(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-retry", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &retryProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Err: transientStreamErr},
			ctesting.Step{Content: "recovered reply"},
		),
	}

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "retry this request"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2 retries", len(calls))
	}
}

func TestConfigureRetryProviderUsesUntilCancelledSetting(t *testing.T) {
	events := make(chan ionsession.Event, 1)
	retryUntilCancelled := true
	provider := &retryProvider{
		FauxProvider: ctesting.NewFauxProvider("openai"),
	}

	wrapped := configureRetryProvider(
		provider,
		&config.Config{RetryUntilCancelled: &retryUntilCancelled},
		events,
	)
	retry, ok := wrapped.(*llm.RetryProvider)
	if !ok {
		t.Fatalf("wrapped provider = %T, want *llm.RetryProvider", wrapped)
	}
	if !retry.Config.RetryForever {
		t.Fatal("RetryForever = false, want true")
	}
	if !retry.Config.RetryForeverTransportOnly {
		t.Fatal("RetryForeverTransportOnly = false, want true")
	}

	retry.Config.OnRetry(llm.RetryEvent{
		Attempt: 1,
		Delay:   2 * time.Second,
		Err:     transientStreamErr,
	})

	select {
	case ev := <-events:
		status, ok := ev.(ionsession.StatusChanged)
		if !ok {
			t.Fatalf("event = %T, want StatusChanged", ev)
		}
		if !strings.Contains(status.Status, "Retrying in 2s") {
			t.Fatalf("status = %q, want retry delay", status.Status)
		}
		if !strings.Contains(status.Status, "Provider error") {
			t.Fatalf("status = %q, want provider error label", status.Status)
		}
		if !strings.Contains(status.Status, "Ctrl+C stops") {
			t.Fatalf("status = %q, want cancel hint", status.Status)
		}
	default:
		t.Fatal("expected retry status event")
	}
}

func TestRetryStatusLabelsTransportErrors(t *testing.T) {
	status := retryStatus(llm.RetryEvent{
		Attempt: 1,
		Delay:   time.Second,
		Err:     syscall.ECONNRESET,
	})
	if !strings.Contains(status, "Network error") {
		t.Fatalf("status = %q, want network error label", status)
	}
}
