package canto

import (
	"context"
	"errors"
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "retry this request"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2 retries", len(calls))
	}
}

func TestRetryRecoveryWaitsThroughToolLoop(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	call := llm.Call{ID: "retry-tool-call", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"printf retry-tool-output"}`
	provider := &retryProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Err: transientStreamErr},
			ctesting.Step{Calls: []llm.Call{call}},
			ctesting.Step{Content: "final after retry tool"},
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if retry, ok := retryProviderInChain(b.llm); ok {
		retry.Config.MinInterval = time.Millisecond
		retry.Config.MaxInterval = time.Millisecond
	}

	if err := b.Session().SubmitTurn(ctx, "retry with tool"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf(
			"provider calls = %d, want transient retry, tool request, final request",
			len(calls),
		)
	}
	if !requestHasMessage(calls[2].Messages, llm.RoleTool, "retry-tool-output") {
		t.Fatalf("final request missing retry tool result: %#v", calls[2].Messages)
	}

	entries, err := storageSession.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.Agent, "final after retry tool") {
		t.Fatalf("final assistant response was not persisted: %#v", entries)
	}
}

func TestRetryExhaustionDoesNotEscalateProviderError(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
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
			ctesting.Step{Err: transientStreamErr},
			ctesting.Step{Err: transientStreamErr},
			ctesting.Step{Err: transientStreamErr},
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	retry, ok := retryProviderInChain(b.llm)
	if !ok {
		t.Fatalf("backend llm = %T, want retry provider in wrapper chain", b.llm)
	}
	retry.Config.MinInterval = time.Millisecond
	retry.Config.MaxInterval = time.Millisecond

	if err := b.Session().SubmitTurn(ctx, "retry until terminal"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	msg := waitForSessionError(t, b.Session().Events())
	waitForTurnFinishedAfterError(t, b.Session().Events())

	if strings.Contains(msg.Err.Error(), "escalation exhausted") {
		t.Fatalf("error leaked agent escalation wording: %v", msg.Err)
	}
	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want only RetryProvider attempts", len(calls))
	}
}

func TestSetConfigUpdatesWrappedRetryProvider(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
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

	provider := &retryProvider{FauxProvider: ctesting.NewFauxProvider("openai")}
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	retry, ok := retryProviderInChain(b.llm)
	if !ok {
		t.Fatalf("backend llm = %T, want retry provider in wrapper chain", b.llm)
	}

	retryUntilCancelled := false
	b.SetConfig(&config.Config{
		Provider:            "openai",
		Model:               "model-a",
		ContextLimit:        100,
		RetryUntilCancelled: &retryUntilCancelled,
	})
	if retry.Config.RetryForever {
		t.Fatal("RetryForever = true, want updated false")
	}
	if !retry.Config.RetryForeverTransportOnly {
		t.Fatal("RetryForeverTransportOnly = false, want true")
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
		if !strings.Contains(status.Status, "transient provider failure") {
			t.Fatalf("status = %q, want provider error detail", status.Status)
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

func TestRetryStatusRedactsErrorDetail(t *testing.T) {
	status := retryStatus(llm.RetryEvent{
		Attempt: 1,
		Delay:   time.Second,
		Err:     errors.New("request failed api_key=sk-secret1234567890"),
	})
	if strings.Contains(status, "sk-secret") {
		t.Fatalf("status leaked secret: %q", status)
	}
	if !strings.Contains(status, "[redacted-secret]") {
		t.Fatalf("status = %q, want redaction marker", status)
	}
}
