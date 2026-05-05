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

func TestCompactUsesManualCompactionHelper(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-compact", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	oldFactory := providerFactory
	provider := &compactProvider{id: "openai"}
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

	compacted, err := b.Compact(ctx)
	if err != nil {
		t.Fatalf("compact: %v", err)
	}
	if !compacted {
		t.Fatal("expected compacted=true")
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume compacted session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.System, "<conversation_summary>") {
		t.Fatalf(
			"expected compacted effective history to include conversation summary, got %#v",
			entries,
		)
	}
	if provider.lastRequest == nil || len(provider.lastRequest.Messages) < 2 ||
		!strings.Contains(
			provider.lastRequest.Messages[1].Content,
			"current user goal and immediate next step",
		) {
		t.Fatalf(
			"summarizer prompt did not include Ion compaction guidance: %#v",
			provider.lastRequest,
		)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	sess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var compactionEvents int
	for _, e := range sess.Events() {
		if e.Type == csession.CompactionTriggered {
			compactionEvents++
		}
	}
	if compactionEvents == 0 {
		t.Fatal("expected at least one durable compaction event")
	}
}

func TestOpenRecoversFromContextOverflowByCompacting(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-overflow", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	provider := &overflowRecoveryProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Err: overflowErr},
			ctesting.Step{Content: "compacted summary"},
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

	if err := b.SubmitTurn(ctx, "overflow recovery please"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3 (overflow, compact, retry)", len(calls))
	}
	if !requestContains(calls[2], "<conversation_summary>") {
		t.Fatalf("retry request was not rebuilt from compacted history: %#v", calls[2].Messages)
	}
	if requestContains(calls[2], strings.Repeat("alpha ", 20)) {
		t.Fatalf("retry request still contains pre-compaction history: %#v", calls[2].Messages)
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume compacted session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.System, "<conversation_summary>") {
		t.Fatalf("expected automatic compaction to add a conversation summary, got %#v", entries)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	sess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var compactionEvents int
	for _, e := range sess.Events() {
		if e.Type == csession.CompactionTriggered {
			compactionEvents++
		}
	}
	if compactionEvents == 0 {
		t.Fatal("expected at least one durable compaction event")
	}
}

func requestContains(req *llm.Request, needle string) bool {
	if req == nil {
		return false
	}
	for _, msg := range req.Messages {
		if strings.Contains(msg.Content, needle) {
			return true
		}
	}
	return false
}

func TestSubmitTurnProactivelyCompactsBeforeOverflow(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-proactive", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	provider := &overflowRecoveryProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Content: "compacted summary"},
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
	b.SetSession(&proactiveUsageSession{
		id: storageSession.ID(),
		meta: storage.Metadata{
			ID:     storageSession.ID(),
			CWD:    "/tmp/ion-proactive",
			Model:  "model-a",
			Branch: "main",
		},
		usageIn:  72,
		usageOut: 8,
	})
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "proactive compaction please"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2 (compact, turn)", len(calls))
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.System, "<conversation_summary>") {
		t.Fatalf("expected proactive compaction to add a conversation summary, got %#v", entries)
	}
	if !entryExists(entries, ionsession.Agent, "recovered reply") {
		t.Fatalf("expected final reply after proactive compaction, got %#v", entries)
	}
}

func TestSubmitTurnStopsWhenProactiveCompactionFails(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-proactive-fail",
		"openai/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
	)

	provider := &overflowRecoveryProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Err: errors.New("compaction provider failed")},
			ctesting.Step{Content: "turn should not run"},
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
	b.SetSession(&proactiveUsageSession{
		id: storageSession.ID(),
		meta: storage.Metadata{
			ID:     storageSession.ID(),
			CWD:    "/tmp/ion-proactive-fail",
			Model:  "model-a",
			Branch: "main",
		},
		usageIn:  72,
		usageOut: 8,
	})
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "do not send this after compaction failure"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	errEvent := waitForSessionError(t, b.Events())
	if !strings.Contains(errEvent.Err.Error(), "compaction provider failed") {
		t.Fatalf("error = %v, want compaction provider failure", errEvent.Err)
	}
	waitForTurnFinishedAfterError(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1 compaction call only", len(calls))
	}
}
