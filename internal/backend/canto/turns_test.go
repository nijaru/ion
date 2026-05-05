package canto

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestSubmitTurnMaterializesLazySession(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := "/tmp/ion-lazy-turn"
	storageSession := storage.NewLazySession(store, cwd, "local-api/model-a", "main")

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

	if storage.IsMaterialized(storageSession) {
		t.Fatal("lazy session materialized during backend open")
	}
	before, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list before submit: %v", err)
	}
	if len(before) != 0 {
		t.Fatalf("sessions before submit = %#v, want none", before)
	}

	if err := b.SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	if !storage.IsMaterialized(storageSession) {
		t.Fatal("lazy session not materialized by submit")
	}
	after, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list after submit: %v", err)
	}
	if len(after) != 1 {
		t.Fatalf("sessions after submit = %d, want 1", len(after))
	}
	if after[0].LastPreview != "hi" {
		t.Fatalf("last preview = %q, want hi", after[0].LastPreview)
	}
}

func TestSubmitTurnUsesCallerContext(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, "/tmp/ion-context", "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
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

	turnCtx, cancel := context.WithCancel(ctx)
	if err := b.SubmitTurn(turnCtx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	var streamCtx context.Context
	select {
	case streamCtx = <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}

	cancel()
	select {
	case <-streamCtx.Done():
	case <-time.After(2 * time.Second):
		t.Fatal("provider stream context was not canceled")
	}
	waitForTurnFinished(t, b.Events())
}

func TestSubmitTurnCancelSuppressesLateAssistant(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-late-cancel",
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &lateSuccessStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
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
	select {
	case <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}
	if err := b.CancelTurn(ctx); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}

	for {
		select {
		case ev := <-b.Events():
			switch msg := ev.(type) {
			case ionsession.AgentMessage:
				t.Fatalf("late assistant reached Ion after cancel: %#v", msg)
			case ionsession.TurnFinished:
				entries, err := storageSession.Entries(ctx)
				if err != nil {
					t.Fatalf("load entries: %v", err)
				}
				for _, entry := range entries {
					if entry.Role == ionsession.Agent {
						t.Fatalf("late assistant persisted after cancel: %#v", entry)
					}
				}
				return
			}
		case <-time.After(2 * time.Second):
			t.Fatal("timed out waiting for canceled turn")
		}
	}
}

func TestSubmitTurnRejectsConcurrentTurn(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-concurrent",
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
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

	turnCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	if err := b.SubmitTurn(turnCtx, "first"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}

	select {
	case <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}

	err = b.SubmitTurn(ctx, "second")
	if err == nil || !strings.Contains(err.Error(), "turn already in progress") {
		t.Fatalf("second SubmitTurn error = %v, want turn already in progress", err)
	}

	cancel()
	waitForTurnFinished(t, b.Events())
}

func TestResumeDoesNotDeadlockWhenBackendNeedsOpen(t *testing.T) {
	b := New()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- b.Resume(ctx, "session-id")
	}()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("expected resume to fail without provider/model")
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("resume appears to deadlock")
	}
}

func TestCloseIsIdempotent(t *testing.T) {
	b := New()

	if err := b.Close(); err != nil {
		t.Fatalf("first close: %v", err)
	}
	if err := b.Close(); err != nil {
		t.Fatalf("second close: %v", err)
	}
}

func TestSubmitTurnToolFailurePersistsForFollowUp(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	call := llm.Call{ID: "tool-call-fail", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"exit 7"}`
	provider := ctesting.NewFauxProvider("local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "handled tool failure"},
		ctesting.Step{Content: "continued"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

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
	b.SetMode(ionsession.ModeYolo)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "run a failing command"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	if err := b.SubmitTurn(ctx, "can you continue after that failure?"); err != nil {
		t.Fatalf("submit follow-up turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3", len(calls))
	}
	postToolRequest := calls[1]
	if !requestHasMessage(postToolRequest.Messages, llm.RoleTool, "exit status 7") {
		t.Fatalf("post-tool request missing failed tool result: %#v", postToolRequest.Messages)
	}
	followUpRequest := calls[2]
	if !requestHasMessage(followUpRequest.Messages, llm.RoleAssistant, "handled tool failure") {
		t.Fatalf(
			"follow-up request missing post-tool assistant reply: %#v",
			followUpRequest.Messages,
		)
	}
	if !requestHasMessage(followUpRequest.Messages, llm.RoleTool, "exit status 7") {
		t.Fatalf("follow-up request missing failed tool result: %#v", followUpRequest.Messages)
	}
}

func TestSubmitTurnProviderErrorLeavesBackendReusable(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-provider-error",
		"openai/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	providerErr := errors.New("provider unavailable")
	provider := ctesting.NewFauxProvider(
		"openai",
		ctesting.Step{Err: providerErr},
		ctesting.Step{Content: "recovered reply"},
	)

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
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "first turn fails"); err != nil {
		t.Fatalf("submit failing turn: %v", err)
	}
	errEvent := waitForSessionError(t, b.Events())
	if !strings.Contains(errEvent.Err.Error(), providerErr.Error()) {
		t.Fatalf("error = %v, want provider error", errEvent.Err)
	}
	waitForTurnFinished(t, b.Events())

	if err := b.SubmitTurn(ctx, "second turn recovers"); err != nil {
		t.Fatalf("submit recovery turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2", len(calls))
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	cantoSess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var terminalErrorFound bool
	for _, ev := range cantoSess.Events() {
		if ev.Type != csession.TurnCompleted {
			continue
		}
		data, ok, err := ev.TurnCompletedData()
		if err != nil {
			t.Fatalf("decode turn completed: %v", err)
		}
		if ok && strings.Contains(data.Error, providerErr.Error()) {
			terminalErrorFound = true
		}
	}
	if !terminalErrorFound {
		t.Fatalf("missing durable provider error terminal event")
	}
}
