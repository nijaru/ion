package canto

import (
	"context"
	"os"
	"path/filepath"
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

func TestOpenLoadsLayeredProjectInstructions(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, ".git"), 0o755); err != nil {
		t.Fatalf("mkdir .git: %v", err)
	}
	nested := filepath.Join(root, "pkg", "feature")
	if err := os.MkdirAll(nested, 0o755); err != nil {
		t.Fatalf("mkdir nested: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("root instruction"), 0o644); err != nil {
		t.Fatalf("write root AGENTS: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "pkg", "AGENTS.md"), []byte("pkg instruction"), 0o644); err != nil {
		t.Fatalf("write pkg AGENTS: %v", err)
	}

	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, nested, "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	mockProvider := ctesting.NewMockProvider("openai", ctesting.Step{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return mockProvider, nil
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

	if err := b.SubmitTurn(ctx, "load instructions"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := mockProvider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	req := calls[0]
	if !requestHasMessage(req.Messages, llm.RoleSystem, "root instruction") {
		t.Fatalf("provider request missing root instruction: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleSystem, "pkg instruction") {
		t.Fatalf("provider request missing nested layer: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleSystem, "## Project Instructions") {
		t.Fatalf("provider request missing project section: %#v", req.Messages)
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

func waitForSessionError(t *testing.T, events <-chan ionsession.Event) ionsession.Error {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before session error")
			}
			if msg, ok := ev.(ionsession.Error); ok {
				return msg
			}
		case <-timeout:
			t.Fatal("timed out waiting for session error")
			return ionsession.Error{}
		}
	}
}

func waitForTurnFinishedAfterError(t *testing.T, events <-chan ionsession.Event) {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before turn finished")
			}
			if _, ok := ev.(ionsession.TurnFinished); ok {
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for turn to finish after session error")
		}
	}
}

func receiveEvent(t *testing.T, events <-chan ionsession.Event) ionsession.Event {
	t.Helper()

	select {
	case ev, ok := <-events:
		if !ok {
			t.Fatal("event stream closed")
		}
		return ev
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for event")
		return nil
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

func specNames(specs []*llm.Spec) []string {
	names := make([]string, 0, len(specs))
	for _, spec := range specs {
		if spec != nil {
			names = append(names, spec.Name)
		}
	}
	return names
}

func entryExists(entries []ionsession.Entry, role ionsession.Role, content string) bool {
	for _, entry := range entries {
		if entry.Role == role && strings.Contains(entry.Content, content) {
			return true
		}
	}
	return false
}

func appendCantoHistory(
	t *testing.T,
	ctx context.Context,
	store storage.Store,
	sessionID string,
	messages ...llm.Message,
) {
	t.Helper()
	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatalf("store %T does not expose Canto history", store)
	}
	for _, msg := range messages {
		if err := cantoStore.Canto().Save(
			ctx,
			csession.NewEvent(sessionID, csession.MessageAdded, msg),
		); err != nil {
			t.Fatalf("append canto history: %v", err)
		}
	}
}
