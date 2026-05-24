package canto

import (
	"context"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
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

func TestSubmitTurnSyncsSessionSettingsBeforeUserMessage(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "openai/model-old", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := llm.NewFauxProvider("openai", llm.FauxStep{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider:        "openai",
		Model:           "model-old",
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

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	if got := calls[0].Model; got != "model-a" {
		t.Fatalf("provider request model = %q, want synced model-a", got)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	cantoSess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	settings, err := cantoSess.EffectiveSettings()
	if err != nil {
		t.Fatalf("effective settings: %v", err)
	}
	if !settings.HasModel ||
		settings.Model.ProviderID != "openai" ||
		settings.Model.Model != "model-a" ||
		settings.ThinkingLevel != "high" {
		t.Fatalf("settings = %#v, want openai/model-a high", settings)
	}

	modelIdx, thinkingIdx, userIdx := -1, -1, -1
	for i, event := range cantoSess.Events() {
		switch event.Type {
		case csession.ModelChanged:
			selection, ok, err := event.ModelSelection()
			if err != nil {
				t.Fatalf("decode model selection: %v", err)
			}
			if ok && selection.ProviderID == "openai" && selection.Model == "model-a" {
				modelIdx = i
			}
		case csession.ThinkingChanged:
			selection, ok, err := event.ThinkingSelection()
			if err != nil {
				t.Fatalf("decode thinking selection: %v", err)
			}
			if ok && selection.Level == "high" {
				thinkingIdx = i
			}
		case csession.MessageAdded:
			var msg llm.Message
			if err := event.UnmarshalData(&msg); err != nil {
				t.Fatalf("decode message: %v", err)
			}
			if msg.Role == llm.RoleUser && msg.TextContent() == "hi" {
				userIdx = i
			}
		}
	}
	if modelIdx < 0 || thinkingIdx < 0 || userIdx < 0 {
		t.Fatalf(
			"event indexes model=%d thinking=%d user=%d, want all present",
			modelIdx,
			thinkingIdx,
			userIdx,
		)
	}
	if modelIdx > userIdx || thinkingIdx > userIdx {
		t.Fatalf(
			"settings events must precede user message: model=%d thinking=%d user=%d",
			modelIdx,
			thinkingIdx,
			userIdx,
		)
	}
}

func TestSubmitTurnRecordsProviderChangeWhenModelMatches(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "openai/shared-model", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	providers := map[string]*llm.FauxProvider{
		"openai":     llm.NewFauxProvider("openai", llm.FauxStep{Content: "one"}),
		"openrouter": llm.NewFauxProvider("openrouter", llm.FauxStep{Content: "two"}),
	}
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return providers[cfg.Provider], nil
	}
	defer func() { providerFactory = oldFactory }()

	first := New()
	first.SetStore(store)
	first.SetSession(storageSession)
	first.SetConfig(&config.Config{Provider: "openai", Model: "shared-model"})
	if err := first.Session().Open(ctx); err != nil {
		t.Fatalf("open first backend: %v", err)
	}
	if err := first.Session().SubmitTurn(ctx, "first"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, first.Session().Events())
	if err := first.Session().Close(); err != nil {
		t.Fatalf("close first backend: %v", err)
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	second := New()
	second.SetStore(store)
	second.SetSession(resumed)
	second.SetConfig(&config.Config{Provider: "openrouter", Model: "shared-model"})
	if err := second.Session().Open(ctx); err != nil {
		t.Fatalf("open second backend: %v", err)
	}
	defer func() { _ = second.Session().Close() }()
	if err := second.Session().SubmitTurn(ctx, "second"); err != nil {
		t.Fatalf("submit second turn: %v", err)
	}
	waitForTurnFinished(t, second.Session().Events())

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	cantoSess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	settings, err := cantoSess.EffectiveSettings()
	if err != nil {
		t.Fatalf("effective settings: %v", err)
	}
	if !settings.HasModel ||
		settings.Model.ProviderID != "openrouter" ||
		settings.Model.Model != "shared-model" {
		t.Fatalf("settings = %#v, want openrouter/shared-model", settings)
	}

	var selections []csession.ModelSelection
	for _, event := range cantoSess.Events() {
		if event.Type != csession.ModelChanged {
			continue
		}
		selection, ok, err := event.ModelSelection()
		if err != nil {
			t.Fatalf("decode model selection: %v", err)
		}
		if ok {
			selections = append(selections, selection)
		}
	}
	if len(selections) != 2 {
		t.Fatalf("model selections = %#v, want openai then openrouter", selections)
	}
	if selections[0].ProviderID != "openai" ||
		selections[1].ProviderID != "openrouter" ||
		selections[0].Model != "shared-model" ||
		selections[1].Model != "shared-model" {
		t.Fatalf("model selections = %#v, want openai then openrouter", selections)
	}
}

func TestSubmitTurnSyncsDefaultCodingToolsBeforeUserMessage(t *testing.T) {
	assertSubmitTurnSyncsToolsBeforeUserMessage(
		t,
		&config.Config{
			Provider: "openai",
			Model:    "model-a",
		},
		"bash,edit,read,write",
	)
}

func TestSubmitTurnSyncsActiveToolModeBeforeUserMessage(t *testing.T) {
	assertSubmitTurnSyncsToolsBeforeUserMessage(
		t,
		&config.Config{
			Provider: "openai",
			Model:    "model-a",
			ToolMode: "read",
		},
		"find,grep,ls,read",
	)
}

func assertSubmitTurnSyncsToolsBeforeUserMessage(
	t *testing.T,
	cfg *config.Config,
	wantTools string,
) {
	t.Helper()

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
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(cfg)
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	if got := strings.Join(specNames(calls[0].Tools), ","); got != wantTools {
		t.Fatalf("provider tools = %q, want %q", got, wantTools)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	cantoSess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	settings, err := cantoSess.EffectiveSettings()
	if err != nil {
		t.Fatalf("effective settings: %v", err)
	}
	if !settings.HasTools || strings.Join(settings.ActiveTools, ",") != wantTools {
		t.Fatalf("settings = %#v, want active tools %q", settings, wantTools)
	}

	toolsIdx, userIdx := -1, -1
	for i, event := range cantoSess.Events() {
		switch event.Type {
		case csession.ToolsChanged:
			selection, ok, err := event.ToolSelection()
			if err != nil {
				t.Fatalf("decode tool selection: %v", err)
			}
			if ok && strings.Join(selection.Names, ",") == wantTools {
				toolsIdx = i
			}
		case csession.MessageAdded:
			var msg llm.Message
			if err := event.UnmarshalData(&msg); err != nil {
				t.Fatalf("decode message: %v", err)
			}
			if msg.Role == llm.RoleUser && msg.TextContent() == "hi" {
				userIdx = i
			}
		}
	}
	if toolsIdx < 0 || userIdx < 0 {
		t.Fatalf("event indexes tools=%d user=%d, want both present", toolsIdx, userIdx)
	}
	if toolsIdx > userIdx {
		t.Fatalf("tool selection must precede user message: tools=%d user=%d", toolsIdx, userIdx)
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
