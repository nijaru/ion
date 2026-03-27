package canto

import (
	"context"
	"strings"
	"testing"
	"time"

	"charm.land/catwalk/pkg/catwalk"
	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type compactProvider struct {
	id string
}

func (p *compactProvider) ID() string { return p.id }

func (p *compactProvider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	return &llm.Response{Content: "condensed summary"}, nil
}

func (p *compactProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	return nil, nil
}

func (p *compactProvider) Models(ctx context.Context) ([]catwalk.Model, error) {
	return nil, nil
}

func (p *compactProvider) CountTokens(ctx context.Context, model string, messages []llm.Message) (int, error) {
	return 10_000, nil
}

func (p *compactProvider) Cost(ctx context.Context, model string, usage llm.Usage) float64 { return 0 }

func (p *compactProvider) Capabilities(model string) llm.Capabilities {
	return llm.DefaultCapabilities()
}

func (p *compactProvider) IsTransient(err error) bool { return false }

func TestProviderAndModelFallBackToEnv(t *testing.T) {
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
	providerFactory = func(providerName string) (llm.Provider, error) {
		switch providerName {
		case "openai":
			return firstProvider, nil
		case "openrouter":
			return secondProvider, nil
		default:
			return oldFactory(providerName)
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
		t.Fatal("second provider request missing first assistant turn")
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
	if !entryExists(entries, ionsession.Assistant, "first reply") {
		t.Fatal("persisted entries missing first assistant turn")
	}
	if !entryExists(entries, ionsession.User, "second question") {
		t.Fatal("persisted entries missing second user turn")
	}
	if !entryExists(entries, ionsession.Assistant, "second reply") {
		t.Fatal("persisted entries missing second assistant turn")
	}
}

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

	for _, msg := range []storage.Assistant{
		{Type: "assistant", Content: []storage.Block{{Type: "text", Text: textPtr(strings.Repeat("alpha ", 60))}}},
		{Type: "assistant", Content: []storage.Block{{Type: "text", Text: textPtr(strings.Repeat("beta ", 60))}}},
		{Type: "assistant", Content: []storage.Block{{Type: "text", Text: textPtr(strings.Repeat("gamma ", 60))}}},
		{Type: "assistant", Content: []storage.Block{{Type: "text", Text: textPtr("recent answer")}}},
	} {
		if err := storageSession.Append(ctx, msg); err != nil {
			t.Fatalf("append history: %v", err)
		}
	}
	if err := storageSession.Append(ctx, storage.User{Type: "user", Content: "recent question"}); err != nil {
		t.Fatalf("append recent user: %v", err)
	}

	oldFactory := providerFactory
	providerFactory = func(providerName string) (llm.Provider, error) {
		if providerName == "openai" {
			return &compactProvider{id: "openai"}, nil
		}
		return oldFactory(providerName)
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
		t.Fatalf("expected compacted effective history to include conversation summary, got %#v", entries)
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
	sess.ForEachEvent(func(e csession.Event) bool {
		if e.Type == csession.CompactionTriggered {
			compactionEvents++
		}
		return true
	})
	if compactionEvents == 0 {
		t.Fatal("expected at least one durable compaction event")
	}
}

func waitForTurnFinished(t *testing.T, events <-chan ionsession.Event) {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before turn finished")
			}
			switch msg := ev.(type) {
			case ionsession.Error:
				t.Fatalf("unexpected session error: %v", msg.Err)
			case ionsession.TurnFinished:
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for turn to finish")
		}
	}
}

func requestHasMessage(messages []llm.Message, role llm.Role, content string) bool {
	for _, msg := range messages {
		if msg.Role == role && strings.Contains(msg.Content, content) {
			return true
		}
	}
	return false
}

func entryExists(entries []ionsession.Entry, role ionsession.Role, content string) bool {
	for _, entry := range entries {
		if entry.Role == role && strings.Contains(entry.Content, content) {
			return true
		}
	}
	return false
}

func textPtr(s string) *string { return &s }
