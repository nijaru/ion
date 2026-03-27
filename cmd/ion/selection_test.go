package main

import (
	"context"
	"testing"

	"github.com/nijaru/canto/memory"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/storage"
)

type metadataStore struct {
	updated storage.SessionInfo
}

func (s *metadataStore) OpenSession(ctx context.Context, cwd, model, branch string) (storage.Session, error) {
	return nil, nil
}

func (s *metadataStore) ResumeSession(ctx context.Context, id string) (storage.Session, error) {
	return nil, nil
}

func (s *metadataStore) ListSessions(ctx context.Context, cwd string) ([]storage.SessionInfo, error) {
	return nil, nil
}

func (s *metadataStore) GetRecentSession(ctx context.Context, cwd string) (*storage.SessionInfo, error) {
	return nil, nil
}

func (s *metadataStore) AddInput(ctx context.Context, cwd, content string) error { return nil }

func (s *metadataStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	return nil, nil
}

func (s *metadataStore) UpdateSession(ctx context.Context, si storage.SessionInfo) error {
	s.updated = si
	return nil
}

func (s *metadataStore) SaveKnowledge(ctx context.Context, item storage.KnowledgeItem) error {
	return nil
}

func (s *metadataStore) SearchKnowledge(ctx context.Context, cwd, query string, limit int) ([]storage.KnowledgeItem, error) {
	return nil, nil
}

func (s *metadataStore) DeleteKnowledge(ctx context.Context, id string) error { return nil }

func (s *metadataStore) CoreStore() *memory.CoreStore { return nil }

func TestBackendForProvider(t *testing.T) {
	cases := []struct {
		name     string
		provider string
		want     string
	}{
		{name: "canto openrouter", provider: "openrouter", want: "canto"},
		{name: "canto anthropic", provider: "anthropic", want: "canto"},
		{name: "acp claude", provider: "claude-pro", want: "acp"},
		{name: "acp gemini", provider: "gemini-advanced", want: "acp"},
		{name: "acp github", provider: "gh-copilot", want: "acp"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			b, err := backendForProvider(tc.provider)
			if err != nil {
				t.Fatalf("backendForProvider(%q) returned error: %v", tc.provider, err)
			}
			if got := b.Name(); got != tc.want {
				t.Fatalf("backendForProvider(%q).Name() = %q, want %q", tc.provider, got, tc.want)
			}
		})
	}
}

func TestDefaultACPCommand(t *testing.T) {
	cases := []struct {
		provider string
		want     string
		ok       bool
	}{
		{provider: "claude-pro", want: "claude --acp", ok: true},
		{provider: "gemini-advanced", want: "gemini --acp", ok: true},
		{provider: "gh-copilot", want: "gh copilot --acp", ok: true},
		{provider: "chatgpt", want: "codex --acp", ok: true},
		{provider: "codex", want: "codex --acp", ok: true},
	}

	for _, tc := range cases {
		t.Run(tc.provider, func(t *testing.T) {
			got, ok := defaultACPCommand(tc.provider)
			if ok != tc.ok {
				t.Fatalf("defaultACPCommand(%q) ok = %v, want %v", tc.provider, ok, tc.ok)
			}
			if got != tc.want {
				t.Fatalf("defaultACPCommand(%q) = %q, want %q", tc.provider, got, tc.want)
			}
		})
	}
}

func TestResolveStartupConfig(t *testing.T) {
	t.Run("requires provider", func(t *testing.T) {
		cfg := &config.Config{}
		if err := resolveStartupConfig(cfg); err != errNoProviderConfigured {
			t.Fatalf("resolveStartupConfig error = %v, want %v", err, errNoProviderConfigured)
		}
	})

	t.Run("subscription provider requires model", func(t *testing.T) {
		cfg := &config.Config{Provider: "claude-pro"}
		if err := resolveStartupConfig(cfg); err != errNoModelConfigured {
			t.Fatalf("resolveStartupConfig error = %v, want %v", err, errNoModelConfigured)
		}
	})

	t.Run("api provider requires model", func(t *testing.T) {
		cfg := &config.Config{Provider: "anthropic"}
		if err := resolveStartupConfig(cfg); err != errNoModelConfigured {
			t.Fatalf("resolveStartupConfig error = %v, want %v", err, errNoModelConfigured)
		}
	})
}

func TestStartupBannerLines(t *testing.T) {
	t.Run("fresh native", func(t *testing.T) {
		got := startupBannerLines("openai", "gpt-4.1", false)
		want := []string{"ion · native · provider=openai · model=gpt-4.1"}
		if len(got) != len(want) {
			t.Fatalf("len(startupBannerLines) = %d, want %d", len(got), len(want))
		}
		for i := range want {
			if got[i] != want[i] {
				t.Fatalf("startupBannerLines[%d] = %q, want %q", i, got[i], want[i])
			}
		}
	})

	t.Run("resumed acp", func(t *testing.T) {
		got := startupBannerLines("chatgpt", "gpt-5.4", true)
		want := []string{
			"--- resumed ---",
			"ion · acp · provider=chatgpt · model=gpt-5.4",
		}
		if len(got) != len(want) {
			t.Fatalf("len(startupBannerLines) = %d, want %d", len(got), len(want))
		}
		for i := range want {
			if got[i] != want[i] {
				t.Fatalf("startupBannerLines[%d] = %q, want %q", i, got[i], want[i])
			}
		}
	})

	t.Run("missing model", func(t *testing.T) {
		got := startupBannerLines("anthropic", "", false)
		want := []string{"ion · native · provider=anthropic"}
		if len(got) != len(want) {
			t.Fatalf("len(startupBannerLines) = %d, want %d", len(got), len(want))
		}
		for i := range want {
			if got[i] != want[i] {
				t.Fatalf("startupBannerLines[%d] = %q, want %q", i, got[i], want[i])
			}
		}
	})

	t.Run("missing provider and model", func(t *testing.T) {
		got := startupBannerLines("", "", false)
		want := []string{"ion · native"}
		if len(got) != len(want) {
			t.Fatalf("len(startupBannerLines) = %d, want %d", len(got), len(want))
		}
		for i := range want {
			if got[i] != want[i] {
				t.Fatalf("startupBannerLines[%d] = %q, want %q", i, got[i], want[i])
			}
		}
	})
}

func TestOpenRuntimeReturnsUnconfiguredBackendWhenSettingsMissing(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}

	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		t.Fatalf("new store: %v", err)
	}

	b, sess, err := openRuntime(context.Background(), store, "/tmp/test", "main", &config.Config{}, "", "")
	if err != nil {
		t.Fatalf("openRuntime returned error: %v", err)
	}
	if got := b.Name(); got != "unconfigured" {
		t.Fatalf("backend name = %q, want %q", got, "unconfigured")
	}
	if sess != nil {
		t.Fatalf("storage session = %#v, want nil", sess)
	}

	msgErr := b.Session().SubmitTurn(context.Background(), "hello")
	if msgErr != errNoProviderConfigured {
		t.Fatalf("submit error = %v, want %v", msgErr, errNoProviderConfigured)
	}
}

func TestOpenRuntimeReturnsUnconfiguredBackendWhenModelMissing(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}

	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		t.Fatalf("new store: %v", err)
	}

	b, sess, err := openRuntime(context.Background(), store, "/tmp/test", "main", &config.Config{Provider: "claude-pro"}, "", "")
	if err != nil {
		t.Fatalf("openRuntime returned error: %v", err)
	}
	if got := b.Name(); got != "unconfigured" {
		t.Fatalf("backend name = %q, want %q", got, "unconfigured")
	}
	if sess != nil {
		t.Fatalf("storage session = %#v, want nil", sess)
	}

	msgErr := b.Session().SubmitTurn(context.Background(), "hello")
	if msgErr != errNoModelConfigured {
		t.Fatalf("submit error = %v, want %v", msgErr, errNoModelConfigured)
	}
}

func TestSessionModelName(t *testing.T) {
	if got := sessionModelName("openrouter", "openai/gpt-5.4"); got != "openrouter/openai/gpt-5.4" {
		t.Fatalf("sessionModelName() = %q, want %q", got, "openrouter/openai/gpt-5.4")
	}
	if got := sessionModelName("claude-pro", ""); got != "claude-pro" {
		t.Fatalf("sessionModelName() = %q, want %q", got, "claude-pro")
	}
}

func TestSyncSessionMetadata(t *testing.T) {
	store := &metadataStore{}

	if err := syncSessionMetadata(context.Background(), store, "sess-123", "openrouter/deepseek/deepseek-v3.2", "feature/handoff"); err != nil {
		t.Fatalf("syncSessionMetadata returned error: %v", err)
	}

	if got := store.updated.ID; got != "sess-123" {
		t.Fatalf("updated session ID = %q, want %q", got, "sess-123")
	}
	if got := store.updated.Model; got != "openrouter/deepseek/deepseek-v3.2" {
		t.Fatalf("updated model = %q, want %q", got, "openrouter/deepseek/deepseek-v3.2")
	}
	if got := store.updated.Branch; got != "feature/handoff" {
		t.Fatalf("updated branch = %q, want %q", got, "feature/handoff")
	}
}
