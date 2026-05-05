package canto

import (
	"context"
	"slices"
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

func TestToolSurfaceFiltersReadModeTools(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "local-api/model-a", "main")
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
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	b.SetMode(ionsession.ModeRead)
	surface := b.ToolSurface()
	want := []string{"glob", "grep", "list", "read"}
	if surface.Count != len(want) {
		t.Fatalf("READ tool count = %d, want %d", surface.Count, len(want))
	}
	if surface.Environment != "" {
		t.Fatalf("READ tool environment = %q, want empty without bash", surface.Environment)
	}
	if strings.Join(surface.Names, ",") != strings.Join(want, ",") {
		t.Fatalf("READ tool surface = %#v, want %#v", surface.Names, want)
	}

	b.SetMode(ionsession.ModeEdit)
	surface = b.ToolSurface()
	want = []string{"bash", "edit", "glob", "grep", "list", "multi_edit", "read", "write"}
	if surface.Count != len(want) {
		t.Fatalf("EDIT tool count = %d, want %d", surface.Count, len(want))
	}
	if surface.Environment != "inherit" {
		t.Fatalf("EDIT tool environment = %q, want inherit", surface.Environment)
	}
	if strings.Join(surface.Names, ",") != strings.Join(want, ",") {
		t.Fatalf("EDIT tool surface = %#v, want %#v", surface.Names, want)
	}

	b.cfg.ToolEnv = "inherit_without_provider_keys"
	surface = b.ToolSurface()
	if surface.Environment != "inherit_without_provider_keys" {
		t.Fatalf(
			"filtered tool environment = %q, want inherit_without_provider_keys",
			surface.Environment,
		)
	}
}

func TestSkillToolSurfaceIsOptIn(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "local-api/model-a", "main")
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
	b.SetConfig(&config.Config{
		Provider:   "local-api",
		Model:      "model-a",
		Endpoint:   "http://localhost:8080/v1",
		SkillTools: "read",
	})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	surface := b.ToolSurface()
	if !slices.Contains(surface.Names, "read_skill") {
		t.Fatalf("tool surface = %#v, want read_skill", surface.Names)
	}
	if surface.Count != 9 {
		t.Fatalf("EDIT tool count = %d, want 9", surface.Count)
	}

	b.SetMode(ionsession.ModeRead)
	surface = b.ToolSurface()
	if !slices.Contains(surface.Names, "read_skill") {
		t.Fatalf("READ tool surface = %#v, want read_skill", surface.Names)
	}
}

func TestSubagentToolSurfaceIsOptIn(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "local-api/model-a", "main")
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
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	if surface := b.ToolSurface(); slices.Contains(surface.Names, "subagent") {
		t.Fatalf("default tool surface = %#v, want no subagent", surface.Names)
	}
	if err := b.Close(); err != nil {
		t.Fatalf("close default backend: %v", err)
	}

	withSubagentSession, err := store.OpenSession(
		ctx,
		t.TempDir(),
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open second session: %v", err)
	}
	withSubagent := New()
	withSubagent.SetStore(store)
	withSubagent.SetSession(withSubagentSession)
	withSubagent.SetConfig(&config.Config{
		Provider:      "local-api",
		Model:         "model-a",
		Endpoint:      "http://localhost:8080/v1",
		SubagentTools: "on",
	})
	if err := withSubagent.Open(ctx); err != nil {
		t.Fatalf("open subagent backend: %v", err)
	}
	defer func() { _ = withSubagent.Close() }()

	surface := withSubagent.ToolSurface()
	if !slices.Contains(surface.Names, "subagent") {
		t.Fatalf("tool surface = %#v, want subagent", surface.Names)
	}
	withSubagent.SetMode(ionsession.ModeRead)
	readSurface := withSubagent.ToolSurface()
	if slices.Contains(readSurface.Names, "subagent") {
		t.Fatalf("READ tool surface = %#v, want subagent hidden", readSurface.Names)
	}
}

func TestSubagentToolExecutesWhenOptedIn(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	call := llm.Call{ID: "subagent-call-1", Type: "function"}
	call.Function.Name = "subagent"
	call.Function.Arguments = `{"agent":"explorer","task":"inspect README","context_mode":"none"}`
	provider := ctesting.NewFauxProvider("local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "child summary"},
		ctesting.Step{Content: "done"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
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
	b.SetConfig(&config.Config{
		Provider:      "local-api",
		Model:         "model-a",
		Endpoint:      "http://localhost:8080/v1",
		SubagentTools: "on",
	})
	b.SetMode(ionsession.ModeYolo)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "delegate a read-only inspection"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3", len(calls))
	}
	if !requestHasMessage(calls[1].Messages, llm.RoleUser, "Task: inspect README") {
		t.Fatalf("child request missing task: %#v", calls[1].Messages)
	}
	if requestHasMessage(calls[1].Messages, llm.RoleUser, "delegate a read-only inspection") {
		t.Fatalf("none-mode child request inherited parent prompt: %#v", calls[1].Messages)
	}
	if !requestHasMessage(calls[2].Messages, llm.RoleTool, "child summary") {
		t.Fatalf("parent continuation missing child tool result: %#v", calls[2].Messages)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("store does not expose canto store")
	}
	parent, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load parent session: %v", err)
	}
	messages, err := parent.EffectiveMessages()
	if err != nil {
		t.Fatalf("parent effective messages: %v", err)
	}
	if !requestHasMessage(messages, llm.RoleTool, "child summary") {
		t.Fatalf("parent history missing child tool result: %#v", messages)
	}
}
