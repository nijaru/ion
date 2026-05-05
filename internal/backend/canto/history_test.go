package canto

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestCrossProviderHandoffPreservesPromptTruth(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	firstProvider := ctesting.NewMockProvider("openai", ctesting.Step{
		Chunks: []llm.Chunk{{Content: "first reply"}},
	})
	secondProvider := ctesting.NewMockProvider("openrouter", ctesting.Step{
		Chunks: []llm.Chunk{{Content: "second reply"}},
	})

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		switch cfg.Provider {
		case "openai":
			return firstProvider, nil
		case "openrouter":
			return secondProvider, nil
		default:
			return oldFactory(ctx, cfg)
		}
	}
	defer func() {
		providerFactory = oldFactory
	}()

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-handoff", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	first := New()
	first.SetStore(store)
	first.SetSession(storageSession)
	first.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})
	if err := first.Open(ctx); err != nil {
		t.Fatalf("open first backend: %v", err)
	}
	defer func() { _ = first.Close() }()

	if err := first.SubmitTurn(ctx, "first question"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, first.Events())
	if err := first.Close(); err != nil {
		t.Fatalf("close first backend: %v", err)
	}

	resumedSession, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}

	second := New()
	second.SetStore(store)
	second.SetSession(resumedSession)
	second.SetConfig(&config.Config{Provider: "openrouter", Model: "model-b"})
	if err := second.Resume(ctx, storageSession.ID()); err != nil {
		t.Fatalf("resume second backend: %v", err)
	}
	defer func() { _ = second.Close() }()

	if got := second.ID(); got != storageSession.ID() {
		t.Fatalf("second backend session ID = %q, want %q", got, storageSession.ID())
	}

	if err := second.SubmitTurn(ctx, "second question"); err != nil {
		t.Fatalf("submit second turn: %v", err)
	}
	waitForTurnFinished(t, second.Events())

	calls := secondProvider.Calls()
	if len(calls) != 1 {
		t.Fatalf("second provider calls = %d, want 1", len(calls))
	}

	req := calls[0]
	if !requestHasMessage(req.Messages, llm.RoleUser, "first question") {
		t.Fatal("second provider request missing first user turn")
	}
	if !requestHasMessage(req.Messages, llm.RoleAssistant, "first reply") {
		t.Fatal("second provider request missing first agent reply")
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "second question") {
		t.Fatal("second provider request missing second user turn")
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume persisted session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.User, "first question") {
		t.Fatal("persisted entries missing first user turn")
	}
	if !entryExists(entries, ionsession.Agent, "first reply") {
		t.Fatal("persisted entries missing first agent turn")
	}
	if !entryExists(entries, ionsession.User, "second question") {
		t.Fatal("persisted entries missing second user turn")
	}
	if !entryExists(entries, ionsession.Agent, "second reply") {
		t.Fatal("persisted entries missing second agent turn")
	}
}

func TestResumedToolSessionSendsValidFollowUpHistory(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	call := llm.Call{ID: "tool-call-1", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"echo ion-smoke"}`
	provider := ctesting.NewMockProvider("local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "done"},
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

	first := New()
	first.SetStore(store)
	first.SetSession(storageSession)
	first.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	first.SetMode(ionsession.ModeYolo)
	if err := first.Open(ctx); err != nil {
		t.Fatalf("open first backend: %v", err)
	}
	defer func() { _ = first.Close() }()

	if err := first.SubmitTurn(ctx, "run the smoke command"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, first.Events())
	if err := first.Close(); err != nil {
		t.Fatalf("close first backend: %v", err)
	}

	resumedSession, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}

	second := New()
	second.SetStore(store)
	second.SetSession(resumedSession)
	second.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	second.SetMode(ionsession.ModeYolo)
	if err := second.Resume(ctx, storageSession.ID()); err != nil {
		t.Fatalf("resume backend: %v", err)
	}
	defer func() { _ = second.Close() }()

	if err := second.SubmitTurn(ctx, "reply continued if the earlier tool result said ion-smoke"); err != nil {
		t.Fatalf("submit follow-up turn: %v", err)
	}
	waitForTurnFinished(t, second.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3", len(calls))
	}
	req := calls[2]
	if !requestHasMessage(req.Messages, llm.RoleUser, "run the smoke command") {
		t.Fatal("follow-up request missing first user turn")
	}
	if !requestHasMessage(req.Messages, llm.RoleAssistant, "done") {
		t.Fatal("follow-up request missing post-tool assistant reply")
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "reply continued") {
		t.Fatal("follow-up request missing new user turn")
	}

	var (
		toolCallIndex   = -1
		toolResultIndex = -1
	)
	for i, msg := range req.Messages {
		if msg.Role == llm.RoleAssistant && len(msg.Calls) == 1 &&
			msg.Calls[0].ID == "tool-call-1" &&
			msg.Calls[0].Function.Name == "bash" {
			toolCallIndex = i
		}
		if msg.Role == llm.RoleTool &&
			msg.ToolID == "tool-call-1" &&
			msg.Name == "bash" &&
			strings.Contains(msg.Content, "ion-smoke") {
			toolResultIndex = i
		}
		if msg.Role == llm.RoleAssistant &&
			strings.TrimSpace(msg.Content) == "" &&
			msg.Reasoning == "" &&
			len(msg.ThinkingBlocks) == 0 &&
			len(msg.Calls) == 0 {
			t.Fatalf("follow-up request contains empty assistant message: %#v", req.Messages)
		}
	}
	if toolCallIndex < 0 {
		t.Fatalf("follow-up request missing assistant tool call: %#v", req.Messages)
	}
	if toolResultIndex < 0 {
		t.Fatalf("follow-up request missing matching tool result: %#v", req.Messages)
	}
	if toolResultIndex < toolCallIndex {
		t.Fatalf(
			"tool result appears before tool call: call=%d result=%d messages=%#v",
			toolCallIndex,
			toolResultIndex,
			req.Messages,
		)
	}
}

func TestProviderHistoryExcludesIonDisplayOnlyEvents(t *testing.T) {
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
	if err := storageSession.Append(ctx, storage.System{
		Type:    "system",
		Content: "UI-only resumed marker must not reach provider",
		TS:      time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append display system: %v", err)
	}
	if err := storageSession.Append(ctx, storage.Status{
		Type:   "status",
		Status: "UI-only retry status must not reach provider",
		TS:     time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append display status: %v", err)
	}
	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleUser, Content: "prior user"},
		llm.Message{Role: llm.RoleAssistant, Content: "prior assistant"},
	)

	provider := ctesting.NewMockProvider("local-api", ctesting.Step{Content: "next"})
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

	if err := b.SubmitTurn(ctx, "new user"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	req := calls[0]
	for _, msg := range req.Messages {
		if strings.Contains(msg.Content, "UI-only") {
			t.Fatalf("provider request contains display-only event: %#v", req.Messages)
		}
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "prior user") {
		t.Fatalf("provider request missing prior user: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleAssistant, "prior assistant") {
		t.Fatalf("provider request missing prior assistant: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "new user") {
		t.Fatalf("provider request missing new user: %#v", req.Messages)
	}
}
