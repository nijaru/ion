package main

import (
	"bytes"
	"context"
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/storage"
)

type metadataStore struct {
	updated  storage.SessionInfo
	sessions []storage.SessionInfo
}

func (s *metadataStore) OpenSession(ctx context.Context, cwd, model, branch string) (storage.Session, error) {
	return nil, nil
}

func (s *metadataStore) ResumeSession(ctx context.Context, id string) (storage.Session, error) {
	return nil, nil
}

func (s *metadataStore) ListSessions(ctx context.Context, cwd string) ([]storage.SessionInfo, error) {
	return s.sessions, nil
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

func (s *metadataStore) Close() error { return nil }

func TestBackendForProvider(t *testing.T) {
	cases := []struct {
		name     string
		provider string
		want     string
	}{
		{name: "canto openrouter", provider: "openrouter", want: "canto"},
		{name: "canto anthropic", provider: "anthropic", want: "canto"},
		{name: "canto together", provider: "together", want: "canto"},
		{name: "canto custom openai", provider: "openai-compatible", want: "canto"},
		{name: "canto local api", provider: "local-api", want: "canto"},
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

func TestNormalizeFlagArgsAcceptsLeadingSeparator(t *testing.T) {
	got, picker := normalizeFlagArgs([]string{"--", "--continue"})
	if picker {
		t.Fatal("normalizeFlagArgs opened picker for continue")
	}
	if len(got) != 1 || got[0] != "--continue" {
		t.Fatalf("normalizeFlagArgs = %#v, want --continue", got)
	}

	plain, picker := normalizeFlagArgs([]string{"--continue"})
	if picker {
		t.Fatal("normalizeFlagArgs opened picker for plain continue")
	}
	if len(plain) != 1 || plain[0] != "--continue" {
		t.Fatalf("normalizeFlagArgs plain = %#v, want unchanged", plain)
	}
}

func TestNormalizeFlagArgsOpensPickerForResumeWithoutID(t *testing.T) {
	got, picker := normalizeFlagArgs([]string{"--resume"})
	if !picker {
		t.Fatal("normalizeFlagArgs did not request resume picker")
	}
	if len(got) != 0 {
		t.Fatalf("normalizeFlagArgs = %#v, want empty args", got)
	}

	withID, picker := normalizeFlagArgs([]string{"--resume", "session-1"})
	if picker {
		t.Fatal("normalizeFlagArgs opened picker for explicit session id")
	}
	if len(withID) != 2 || withID[0] != "--resume" || withID[1] != "session-1" {
		t.Fatalf("normalizeFlagArgs explicit = %#v, want resume session-1", withID)
	}
}

func TestRecentSessionForContinueSkipsEmptyAndSlashOnlySessions(t *testing.T) {
	store := &metadataStore{sessions: []storage.SessionInfo{
		{ID: "empty"},
		{ID: "slash", LastPreview: "/resume"},
		{ID: "slash-title", Title: "/model", LastPreview: "hi"},
		{ID: "real", LastPreview: "hello"},
	}}

	recent, err := recentSessionForContinue(context.Background(), store, "/tmp/test")
	if err != nil {
		t.Fatalf("recent session: %v", err)
	}
	if recent == nil || recent.ID != "real" {
		t.Fatalf("recent = %#v, want real", recent)
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

	t.Run("custom endpoint provider requires endpoint", func(t *testing.T) {
		cfg := &config.Config{Provider: "openai-compatible", Model: "test-model"}
		err := resolveStartupConfig(cfg)
		if err == nil || err.Error() != "Custom API requires endpoint configuration" {
			t.Fatalf("resolveStartupConfig error = %v", err)
		}
	})

	t.Run("custom endpoint provider accepts endpoint override", func(t *testing.T) {
		cfg := &config.Config{Provider: "openai-compatible", Model: "test-model", Endpoint: "https://example.com/v1"}
		if err := resolveStartupConfig(cfg); err != nil {
			t.Fatalf("resolveStartupConfig error = %v", err)
		}
	})
}

func TestStartupBannerLines(t *testing.T) {
	t.Run("fresh native", func(t *testing.T) {
		got := startupBannerLines("v0.0.0", "openai", "gpt-4.1", false)
		want := []string{"ion v0.0.0"}
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
		got := startupBannerLines("v0.0.0", "chatgpt", "gpt-5.4", true)
		want := []string{"ion v0.0.0"}
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
		got := startupBannerLines("v0.0.0", "anthropic", "", false)
		want := []string{"ion v0.0.0"}
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
		got := startupBannerLines("v0.0.0", "", "", false)
		want := []string{"ion v0.0.0"}
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

func TestPrintStartupPlacesResumeMarkerAfterHeaderBeforeTranscript(t *testing.T) {
	var buf bytes.Buffer
	printStartup(
		&buf,
		[]string{"ion v0.0.0", "13 tools registered"},
		"~/repo • main",
		true,
		[]string{"› hi", "", "• hello"},
	)

	out := ansi.Strip(buf.String())
	workspaceIdx := strings.Index(out, "~/repo")
	resumedIdx := strings.Index(out, "--- resumed ---")
	transcriptIdx := strings.Index(out, "› hi")
	if workspaceIdx < 0 || resumedIdx < 0 || transcriptIdx < 0 {
		t.Fatalf("startup output missing expected parts: %q", out)
	}
	if !(workspaceIdx < resumedIdx && resumedIdx < transcriptIdx) {
		t.Fatalf("resume marker order is wrong: %q", out)
	}
	if !strings.Contains(out, "--- resumed ---\n\n› hi\n\n• hello") {
		t.Fatalf("startup output should separate resumed marker and transcript entries: %q", out)
	}
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

func TestOpenRuntimeReturnsUnconfiguredBackendForInvalidProviderConfig(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}

	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	defer store.Close()

	cfg := &config.Config{Provider: "local-api", Model: "qwen-test"}
	b, sess, err := openRuntime(context.Background(), store, "/tmp/test", "main", cfg, "", "")
	if err != nil {
		t.Fatalf("openRuntime returned error: %v", err)
	}
	if got := b.Name(); got != "unconfigured" {
		t.Fatalf("backend name = %q, want %q", got, "unconfigured")
	}
	if sess != nil {
		t.Fatalf("storage session = %#v, want nil", sess)
	}
	sessions, err := store.ListSessions(context.Background(), "/tmp/test")
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 0 {
		t.Fatalf("sessions = %#v, want none before a model-visible turn", sessions)
	}

	err = b.Session().SubmitTurn(context.Background(), "hello")
	if err == nil || !strings.Contains(err.Error(), "Local API requires endpoint configuration") {
		t.Fatalf("submit error = %v, want endpoint configuration error", err)
	}
}

func TestOpenRuntimeResumeWithInvalidProviderConfigLoadsExistingSessionOnly(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}

	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	defer store.Close()

	ctx := context.Background()
	seed, err := store.OpenSession(ctx, "/tmp/test", "local-api/qwen-test", "main")
	if err != nil {
		t.Fatalf("open seed session: %v", err)
	}
	seedID := seed.ID()
	if err := seed.Append(ctx, storage.System{Type: "system", Content: "seeded", TS: 1}); err != nil {
		t.Fatalf("append seed event: %v", err)
	}
	if err := seed.Close(); err != nil {
		t.Fatalf("close seed session: %v", err)
	}

	cfg := &config.Config{Provider: "local-api", Model: "qwen-test"}
	b, sess, err := openRuntime(ctx, store, "/tmp/test", "feature/resume", cfg, "", seedID)
	if err != nil {
		t.Fatalf("openRuntime returned error: %v", err)
	}
	defer sess.Close()
	if got := b.Name(); got != "unconfigured" {
		t.Fatalf("backend name = %q, want %q", got, "unconfigured")
	}
	if sess == nil {
		t.Fatal("storage session = nil, want resumed session")
	}
	if got := sess.ID(); got != seedID {
		t.Fatalf("storage session ID = %q, want %q", got, seedID)
	}
	if got := b.Session().ID(); got != seedID {
		t.Fatalf("agent session ID = %q, want %q", got, seedID)
	}
	if got := b.Session().Meta()["cwd"]; got != "/tmp/test" {
		t.Fatalf("agent cwd meta = %q, want /tmp/test", got)
	}

	sessions, err := store.ListSessions(ctx, "/tmp/test")
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 || sessions[0].ID != seedID {
		t.Fatalf("sessions = %#v, want only resumed seed session", sessions)
	}
}

func TestOpenRuntimeWithLazySessionDoesNotCreateRecentSession(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}

	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	defer store.Close()

	cfg := &config.Config{Provider: "ollama", Model: "qwen-test"}
	b, sess, err := openRuntime(context.Background(), store, "/tmp/test", "main", cfg, "", "")
	if err != nil {
		t.Fatalf("openRuntime returned error: %v", err)
	}
	defer closeRuntimeHandles(b.Session(), sess, nil)
	if b.Name() != "canto" {
		t.Fatalf("backend name = %q, want canto", b.Name())
	}
	if sess == nil {
		t.Fatal("storage session = nil, want lazy session")
	}
	if storage.IsMaterialized(sess) {
		t.Fatal("fresh runtime materialized session before a model-visible turn")
	}
	sessions, err := store.ListSessions(context.Background(), "/tmp/test")
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 0 {
		t.Fatalf("sessions = %#v, want none before a model-visible turn", sessions)
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
