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
	"github.com/nijaru/ion/internal/storage"
)

func TestToolSurfaceReportsNativeTrustedTools(t *testing.T) {
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	surface := b.ToolSurface()
	want := []string{"bash", "edit", "find", "grep", "ls", "read", "write"}
	if surface.Count != len(want) {
		t.Fatalf("tool count = %d, want %d", surface.Count, len(want))
	}
	if strings.Join(surface.Names, ",") != strings.Join(want, ",") {
		t.Fatalf("tool surface = %#v, want %#v", surface.Names, want)
	}
	if surface.Mode != "coding" {
		t.Fatalf("tool mode = %q, want coding", surface.Mode)
	}
	if strings.Join(surface.ActiveNames, ",") != strings.Join(want, ",") {
		t.Fatalf("active tools = %#v, want %#v", surface.ActiveNames, want)
	}
	if surface.Environment != "inherit" {
		t.Fatalf("tool environment = %q, want inherit", surface.Environment)
	}
	if surface.Sandbox != "" {
		t.Fatalf("tool sandbox = %q, want empty while sandbox is parked", surface.Sandbox)
	}

	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
		ToolEnv:  "inherit_without_provider_keys",
	})
	surface = b.ToolSurface()
	if surface.Environment != "inherit_without_provider_keys" {
		t.Fatalf(
			"filtered tool environment = %q, want inherit_without_provider_keys",
			surface.Environment,
		)
	}

	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
		ToolMode: "read",
	})
	surface = b.ToolSurface()
	if surface.Mode != "read" {
		t.Fatalf("tool mode = %q, want read", surface.Mode)
	}
	if got := strings.Join(surface.ActiveNames, ","); got != "find,grep,ls,read" {
		t.Fatalf("active tools = %q, want read-only tools", got)
	}
}

func TestToolSurfaceIgnoresParkedSandboxEnv(t *testing.T) {
	t.Setenv("ION_SANDBOX", "auto")

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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if surface := b.ToolSurface(); surface.Sandbox != "" {
		t.Fatalf("tool sandbox = %q, want empty while sandbox is parked", surface.Sandbox)
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	surface := b.ToolSurface()
	if !slices.Contains(surface.Names, "read_skill") {
		t.Fatalf("tool surface = %#v, want read_skill", surface.Names)
	}
	if surface.Count != 8 {
		t.Fatalf("tool count = %d, want 8", surface.Count)
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	if surface := b.ToolSurface(); slices.Contains(surface.Names, "subagent") {
		t.Fatalf("default tool surface = %#v, want no subagent", surface.Names)
	}
	if err := b.Session().Close(); err != nil {
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
	if err := withSubagent.Session().Open(ctx); err != nil {
		t.Fatalf("open subagent backend: %v", err)
	}
	defer func() { _ = withSubagent.Session().Close() }()

	surface := withSubagent.ToolSurface()
	if !slices.Contains(surface.Names, "subagent") {
		t.Fatalf("tool surface = %#v, want subagent", surface.Names)
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
	provider := ctesting.NewFauxProvider(
		"local-api",
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "delegate a read-only inspection"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

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
