package canto

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestBackendSteerTurnQueuesWithoutActiveTurn(t *testing.T) {
	backend := New()
	result, err := backendSteeringSession(t, backend).SteerTurn(t.Context(), "later")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringQueued {
		t.Fatalf("outcome = %q, want queued", result.Outcome)
	}
}

func TestBackendSteerTurnQueuesBeforeCantoTurnAcceptance(t *testing.T) {
	turn := newTurnState()
	turn.start(func() {})
	backend := New()
	backend.turn = turn

	result, err := backendSteeringSession(t, backend).SteerTurn(t.Context(), "too early")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringQueued {
		t.Fatalf("outcome = %q, want queued before Canto turn acceptance", result.Outcome)
	}
}

func TestBackendSteeringAppearsInNextProviderRequestAfterTool(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	call := llm.Call{ID: "tool-call-steer", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"sleep 0.3; echo steering-ready"}`
	provider := llm.NewFauxProvider(
		"local-api",
		llm.FauxStep{Calls: []llm.Call{call}},
		llm.FauxStep{Content: "done"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "run the slow command"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForToolStarted(t, b.Session().Events(), "bash")

	result, err := backendSteeringSession(t, b).SteerTurn(ctx, "use the smaller test")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringAccepted {
		t.Fatalf("steering outcome = %q, want accepted", result.Outcome)
	}

	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2", len(calls))
	}
	if requestHasMessage(calls[0].Messages, llm.RoleUser, "use the smaller test") {
		t.Fatalf("first provider request unexpectedly contains steering: %#v", calls[0].Messages)
	}
	if !requestHasMessage(calls[1].Messages, llm.RoleUser, "use the smaller test") {
		t.Fatalf("second provider request missing steering: %#v", calls[1].Messages)
	}
}

func TestBackendSteeringDoesNotRequireActiveTool(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingFirstStreamProvider{
		FauxProvider: llm.NewFauxProvider(
			"local-api",
			llm.FauxStep{Content: "first answer"},
			llm.FauxStep{Content: "after steering"},
		),
		entered: make(chan struct{}),
		release: make(chan struct{}),
	}

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "start"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	select {
	case <-provider.entered:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for first provider request")
	}

	result, err := backendSteeringSession(t, b).SteerTurn(ctx, "steer without tool")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringAccepted {
		t.Fatalf("steering outcome = %q, want accepted", result.Outcome)
	}
	close(provider.release)

	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2", len(calls))
	}
	if requestHasMessage(calls[0].Messages, llm.RoleUser, "steer without tool") {
		t.Fatalf("first provider request unexpectedly contains steering: %#v", calls[0].Messages)
	}
	if !requestHasMessage(calls[1].Messages, llm.RoleUser, "steer without tool") {
		t.Fatalf("second provider request missing steering: %#v", calls[1].Messages)
	}
}

func TestBackendCancelClearsQueuedSteering(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingFirstStreamProvider{
		FauxProvider: llm.NewFauxProvider(
			"local-api",
			llm.FauxStep{Content: "next answer"},
			llm.FauxStep{Content: "stale steering answer"},
		),
		entered: make(chan struct{}),
		release: make(chan struct{}),
	}

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "start"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	select {
	case <-provider.entered:
	case <-time.After(backendEventWaitTimeout):
		t.Fatal("timed out waiting for first provider request")
	}

	result, err := backendSteeringSession(t, b).SteerTurn(ctx, "do not leak")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringAccepted {
		t.Fatalf("steering outcome = %q, want accepted", result.Outcome)
	}
	if err := b.Session().CancelTurn(ctx); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	if err := b.Session().SubmitTurn(ctx, "next"); err != nil {
		t.Fatalf("submit next turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1 after canceled turn; calls=%#v", len(calls), calls)
	}
	if requestHasMessage(calls[0].Messages, llm.RoleUser, "do not leak") {
		t.Fatalf("next turn provider request leaked canceled steering: %#v", calls[0].Messages)
	}
}

func backendSteeringSession(t *testing.T, b *Backend) ionsession.SteeringSession {
	t.Helper()
	steering, ok := b.Session().(ionsession.SteeringSession)
	if !ok {
		t.Fatal("canto session does not implement steering")
	}
	return steering
}

func waitForToolStarted(t *testing.T, events <-chan ionsession.Event, toolName string) {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before tool start")
			}
			switch msg := ev.(type) {
			case ionsession.Error:
				t.Fatalf("unexpected session error: %v", msg.Err)
			case ionsession.ToolCallStarted:
				if msg.ToolName == toolName {
					return
				}
			}
		case <-timeout:
			t.Fatalf("timed out waiting for %s tool start", toolName)
		}
	}
}

type blockingFirstStreamProvider struct {
	*llm.FauxProvider
	entered chan struct{}
	release chan struct{}
	once    sync.Once
}

func (p *blockingFirstStreamProvider) Stream(
	ctx context.Context,
	req *llm.Request,
) (llm.Stream, error) {
	var wait bool
	p.once.Do(func() {
		close(p.entered)
		wait = true
	})
	if wait {
		select {
		case <-p.release:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	return p.FauxProvider.Stream(ctx, req)
}
