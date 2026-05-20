package canto

import (
	"context"
	"sync/atomic"
	"testing"
	"time"

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

func TestSetConfigUpdatesOpenReasoningProcessor(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := llm.NewFauxProvider("openai", llm.FauxStep{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return reasoningFauxProvider{Provider: provider}, nil
	}
	defer func() { providerFactory = oldFactory }()

	var gotReasoning string
	restoreObserver := SetProviderRequestObserverForTest(func(provider string, req *llm.Request) {
		gotReasoning = req.ReasoningEffort
	})
	defer restoreObserver()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider:        "openai",
		Model:           "model-a",
		ReasoningEffort: "low",
	})
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	b.SetConfig(&config.Config{
		Provider:        "openai",
		Model:           "model-a",
		ReasoningEffort: "high",
	})
	if err := b.Session().SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	if gotReasoning != "high" {
		t.Fatalf("reasoning effort = %q, want high from latest SetConfig", gotReasoning)
	}
}

func TestCancelTurnDuringOpenDoesNotWaitForProviderSetup(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	providerStarted := make(chan struct{})
	releaseProvider := make(chan struct{})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		close(providerStarted)
		select {
		case <-releaseProvider:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		return llm.NewFauxProvider("openai", llm.FauxStep{Content: "ok"}), nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})

	openDone := make(chan error, 1)
	go func() {
		openDone <- b.Session().Open(ctx)
	}()

	select {
	case <-providerStarted:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for provider setup")
	}

	cancelDone := make(chan error, 1)
	go func() {
		cancelDone <- b.Session().CancelTurn(t.Context())
	}()

	select {
	case err := <-cancelDone:
		if err != nil {
			t.Fatalf("cancel turn: %v", err)
		}
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("CancelTurn waited for provider setup")
	}

	close(releaseProvider)
	select {
	case err := <-openDone:
		if err != nil {
			t.Fatalf("open backend: %v", err)
		}
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for Open to finish")
	}
	defer func() { _ = b.Session().Close() }()
}

func TestSetConfigDuringOpenDoesNotRaceWithProviderPublish(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	providerStarted := make(chan struct{})
	releaseProvider := make(chan struct{})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		close(providerStarted)
		select {
		case <-releaseProvider:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		return llm.NewFauxProvider("openai", llm.FauxStep{Content: "ok"}), nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})

	openDone := make(chan error, 1)
	go func() {
		openDone <- b.Session().Open(ctx)
	}()

	select {
	case <-providerStarted:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for provider setup")
	}

	var stop atomic.Bool
	configDone := make(chan struct{})
	go func() {
		defer close(configDone)
		for !stop.Load() {
			b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})
		}
	}()

	close(releaseProvider)
	select {
	case err := <-openDone:
		if err != nil {
			t.Fatalf("open backend: %v", err)
		}
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for Open to finish")
	}
	stop.Store(true)
	select {
	case <-configDone:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for SetConfig loop")
	}
	defer func() { _ = b.Session().Close() }()
}

type reasoningFauxProvider struct {
	llm.Provider
}

func (p reasoningFauxProvider) Capabilities(model string) llm.Capabilities {
	caps := llm.DefaultCapabilities()
	caps.Reasoning = llm.ReasoningCapabilities{
		Kind:       llm.ReasoningKindEffort,
		Efforts:    []string{"minimal", "low", "medium", "high"},
		CanDisable: true,
	}
	return caps
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

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
