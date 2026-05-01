package main

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/storage"
)

type metadataStore struct {
	updated  storage.SessionInfo
	sessions []storage.SessionInfo
	listErr  error
}

func (s *metadataStore) OpenSession(ctx context.Context, cwd, model, branch string) (storage.Session, error) {
	return nil, nil
}

func (s *metadataStore) ResumeSession(ctx context.Context, id string) (storage.Session, error) {
	return nil, nil
}

func (s *metadataStore) ListSessions(ctx context.Context, cwd string) ([]storage.SessionInfo, error) {
	if s.listErr != nil {
		return nil, s.listErr
	}
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
		wantErr  string
	}{
		{name: "canto openrouter", provider: "openrouter", want: "canto"},
		{name: "canto anthropic", provider: "anthropic", want: "canto"},
		{name: "canto together", provider: "together", want: "canto"},
		{name: "canto custom openai", provider: "openai-compatible", want: "canto"},
		{name: "canto local api", provider: "local-api", want: "canto"},
		{name: "acp claude", provider: "claude-pro", wantErr: "ACP providers is disabled"},
		{name: "acp gemini", provider: "gemini-advanced", wantErr: "ACP providers is disabled"},
		{name: "acp github", provider: "gh-copilot", wantErr: "ACP providers is disabled"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			b, err := backendForProvider(tc.provider)
			if tc.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
					t.Fatalf("backendForProvider(%q) error = %v, want %q", tc.provider, err, tc.wantErr)
				}
				if b != nil {
					t.Fatalf("backendForProvider(%q) backend = %#v, want nil", tc.provider, b)
				}
				return
			}
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

	short, picker := normalizeFlagArgs([]string{"-c"})
	if picker {
		t.Fatal("normalizeFlagArgs opened picker for short continue")
	}
	if len(short) != 1 || short[0] != "-c" {
		t.Fatalf("normalizeFlagArgs short = %#v, want -c", short)
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

	short, picker := normalizeFlagArgs([]string{"-r"})
	if !picker {
		t.Fatal("normalizeFlagArgs did not request resume picker for -r")
	}
	if len(short) != 0 {
		t.Fatalf("normalizeFlagArgs short = %#v, want empty args", short)
	}

	shortWithID, picker := normalizeFlagArgs([]string{"-r", "session-1"})
	if picker {
		t.Fatal("normalizeFlagArgs opened picker for explicit short session id")
	}
	if len(shortWithID) != 2 || shortWithID[0] != "-r" || shortWithID[1] != "session-1" {
		t.Fatalf("normalizeFlagArgs short explicit = %#v, want -r session-1", shortWithID)
	}
}

func TestNormalizeFlagArgsKeepsModelAndThinkingValues(t *testing.T) {
	got, picker := normalizeFlagArgs([]string{"-p", "--model", "local-model", "--thinking", "high", "hello"})
	want := []string{"-p", "--model", "local-model", "--thinking", "high", "--", "hello"}
	if picker {
		t.Fatal("normalizeFlagArgs opened picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}

	short, picker := normalizeFlagArgs([]string{"-p", "-m", "local-model", "hello"})
	shortWant := []string{"-p", "-m", "local-model", "--", "hello"}
	if picker {
		t.Fatal("normalizeFlagArgs opened picker for short model")
	}
	if !slices.Equal(short, shortWant) {
		t.Fatalf("normalizeFlagArgs short = %#v, want %#v", short, shortWant)
	}
}

func TestApplyCLIConfigOverrides(t *testing.T) {
	cfg := &config.Config{}
	applyCLIConfigOverrides(cfg, "", "openai/gpt-4.1", "high")
	if cfg.Provider != "openai" || cfg.Model != "gpt-4.1" || cfg.ReasoningEffort != "high" {
		t.Fatalf("cfg = %#v, want openai/gpt-4.1 high", cfg)
	}

	cfg = &config.Config{Provider: "openrouter"}
	applyCLIConfigOverrides(cfg, "", "openai/gpt-4.1", "")
	if cfg.Provider != "openrouter" || cfg.Model != "openai/gpt-4.1" {
		t.Fatalf("cfg = %#v, want openrouter with slash model preserved", cfg)
	}

	applyCLIConfigOverrides(cfg, "local-api", "qwen3.6:27b", "")
	if cfg.Provider != "local-api" || cfg.Model != "qwen3.6:27b" {
		t.Fatalf("cfg = %#v, want local-api qwen model", cfg)
	}

	cfg = &config.Config{
		Provider:               "openrouter",
		Model:                  "openai/gpt-5.4",
		FastModel:              "google/gemini-2.0-flash-lite-001",
		FastReasoningEffort:    "low",
		SummaryModel:           "google/gemini-2.0-flash-lite-001",
		SummaryReasoningEffort: "low",
	}
	applyCLIConfigOverrides(cfg, "local-api", "", "")
	if cfg.Provider != "local-api" ||
		cfg.Model != "" ||
		cfg.FastModel != "" ||
		cfg.FastReasoningEffort != "" ||
		cfg.SummaryModel != "" ||
		cfg.SummaryReasoningEffort != "" {
		t.Fatalf("cfg = %#v, want provider-only override to clear stale provider-scoped presets", cfg)
	}

	cfg = &config.Config{
		Provider:  "local-api",
		Model:     "qwen3.6:27b",
		FastModel: "qwen3.6:27b-fast",
	}
	applyCLIConfigOverrides(cfg, "openrouter", "tencent/hy3-preview:free", "")
	if cfg.Provider != "openrouter" ||
		cfg.Model != "tencent/hy3-preview:free" ||
		cfg.FastModel != "" {
		t.Fatalf("cfg = %#v, want explicit provider/model override to clear stale fast preset", cfg)
	}
}

func TestStartupRuntimeConfigHonorsPersistedFastPreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := os.MkdirAll(filepath.Join(home, ".ion"), 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := config.SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}

	runtimeCfg, preset, err := startupRuntimeConfig(context.Background(), &config.Config{
		Provider:            "openai",
		Model:               "gpt-4.1",
		ReasoningEffort:     "high",
		FastModel:           "gpt-4.1-mini",
		FastReasoningEffort: "low",
	}, "", false)
	if err != nil {
		t.Fatalf("startup runtime config: %v", err)
	}
	if preset != "fast" {
		t.Fatalf("preset = %q, want fast", preset)
	}
	if runtimeCfg.Model != "gpt-4.1-mini" || runtimeCfg.ReasoningEffort != "low" {
		t.Fatalf("runtime cfg = %#v, want fast model and low reasoning", runtimeCfg)
	}
}

func TestStartupRuntimeConfigForcesPrimaryForExplicitRuntimeOverride(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := os.MkdirAll(filepath.Join(home, ".ion"), 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := config.SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}

	runtimeCfg, preset, err := startupRuntimeConfig(context.Background(), &config.Config{
		Provider:            "openrouter",
		Model:               "tencent/hy3-preview:free",
		FastModel:           "google/gemini-2.0-flash-lite-001",
		FastReasoningEffort: "low",
	}, "", true)
	if err != nil {
		t.Fatalf("startup runtime config: %v", err)
	}
	if preset != "primary" {
		t.Fatalf("preset = %q, want primary", preset)
	}
	if runtimeCfg.Model != "tencent/hy3-preview:free" {
		t.Fatalf("runtime model = %q, want explicit primary model", runtimeCfg.Model)
	}

	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("active_preset = %#v, want persisted fast unchanged", state.ActivePreset)
	}
}

func TestStartupRuntimeConfigFallsBackWhenPersistedFastIsNotConfigured(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := os.MkdirAll(filepath.Join(home, ".ion"), 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := config.SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}

	runtimeCfg, preset, err := startupRuntimeConfig(context.Background(), &config.Config{
		Provider: "openai",
		Model:    "gpt-4.1",
	}, "", false)
	if err != nil {
		t.Fatalf("startup runtime config: %v", err)
	}
	if preset != "primary" {
		t.Fatalf("preset = %q, want primary fallback", preset)
	}
	if runtimeCfg.Model != "gpt-4.1" {
		t.Fatalf("runtime model = %q, want primary model", runtimeCfg.Model)
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

func TestStartupSessionIDContinuesConversationSession(t *testing.T) {
	store := &metadataStore{sessions: []storage.SessionInfo{
		{ID: "empty"},
		{ID: "real", LastPreview: "hello"},
	}}

	id, err := startupSessionID(context.Background(), store, "/tmp/test", "", "", true)
	if err != nil {
		t.Fatalf("startupSessionID returned error: %v", err)
	}
	if id != "real" {
		t.Fatalf("session ID = %q, want real", id)
	}
}

func TestStartupSessionIDRejectsMissingContinueSession(t *testing.T) {
	store := &metadataStore{}

	id, err := startupSessionID(context.Background(), store, "/tmp/test", "", "", true)
	if err == nil || !strings.Contains(err.Error(), "no conversation session to continue") {
		t.Fatalf("startupSessionID id=%q error=%v, want missing continue error", id, err)
	}
}

func TestStartupSessionIDPropagatesContinueLookupError(t *testing.T) {
	store := &metadataStore{listErr: os.ErrPermission}

	id, err := startupSessionID(context.Background(), store, "/tmp/test", "", "", true)
	if err == nil || !strings.Contains(err.Error(), "failed to find recent session") {
		t.Fatalf("startupSessionID id=%q error=%v, want lookup error", id, err)
	}
}

func TestStartupSessionIDPrefersExplicitResume(t *testing.T) {
	store := &metadataStore{sessions: []storage.SessionInfo{{ID: "recent", LastPreview: "hello"}}}

	id, err := startupSessionID(context.Background(), store, "/tmp/test", "explicit", "", true)
	if err != nil {
		t.Fatalf("startupSessionID returned error: %v", err)
	}
	if id != "explicit" {
		t.Fatalf("session ID = %q, want explicit", id)
	}

	id, err = startupSessionID(context.Background(), store, "/tmp/test", "", "short", true)
	if err != nil {
		t.Fatalf("startupSessionID short returned error: %v", err)
	}
	if id != "short" {
		t.Fatalf("session ID = %q, want short", id)
	}
}

func TestResolveStartupConfig(t *testing.T) {
	t.Run("requires provider", func(t *testing.T) {
		cfg := &config.Config{}
		if err := resolveStartupConfig(cfg); err != errNoProviderConfigured {
			t.Fatalf("resolveStartupConfig error = %v, want %v", err, errNoProviderConfigured)
		}
		if strings.Contains(errNoProviderConfigured.Error(), "Ctrl+") {
			t.Fatalf("provider error mentions stale hotkey: %v", errNoProviderConfigured)
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
		if strings.Contains(errNoModelConfigured.Error(), "Ctrl+") {
			t.Fatalf("model error mentions stale hotkey: %v", errNoModelConfigured)
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
	if !strings.HasSuffix(out, "• hello\n\n") {
		t.Fatalf("startup output should leave one blank row before shell: %q", out)
	}
}

func TestPrintStartupLeavesBlankRowBeforeFreshShell(t *testing.T) {
	var buf bytes.Buffer
	printStartup(
		&buf,
		[]string{"ion v0.0.0", "Tools: 9 registered"},
		"~/repo • main",
		false,
		nil,
	)

	out := ansi.Strip(buf.String())
	if !strings.HasSuffix(out, "~/repo • main\n\n") {
		t.Fatalf("fresh startup output should leave one blank row before shell: %q", out)
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

func TestOpenRuntimeDisablesACPProvidersDuringCoreLoopStabilization(t *testing.T) {
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

	cfg := &config.Config{Provider: "claude-pro", Model: "sonnet"}
	b, sess, err := openRuntime(context.Background(), store, "/tmp/test", "main", cfg, "", "")
	if err == nil {
		t.Fatal("openRuntime returned nil error, want ACP disabled error")
	}
	if !strings.Contains(err.Error(), "ACP providers is disabled while Ion stabilizes the P1 core agent loop") {
		t.Fatalf("openRuntime error = %v, want ACP disabled error", err)
	}
	if b != nil {
		t.Fatalf("backend = %#v, want nil", b)
	}
	if sess != nil {
		t.Fatalf("storage session = %#v, want nil", sess)
	}
}

func TestOpenRuntimeIgnoresExternalPolicyConfigDuringCoreLoopStabilization(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	policyPath := t.TempDir() + "/policy.yaml"
	if err := os.WriteFile(policyPath, []byte("rules:\n  - {}\n"), 0o600); err != nil {
		t.Fatalf("write policy: %v", err)
	}

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}

	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	defer store.Close()

	cfg := &config.Config{
		Provider:   "local-api",
		Model:      "test-model",
		Endpoint:   "https://example.com/v1",
		PolicyPath: policyPath,
	}
	b, sess, err := openRuntime(context.Background(), store, "/tmp/test", "main", cfg, "", "")
	if err != nil {
		t.Fatalf("openRuntime returned error: %v", err)
	}
	defer closeRuntimeHandles(b.Session(), sess, nil)
	if b == nil || sess == nil {
		t.Fatalf("runtime = (%#v, %#v), want configured backend and lazy session", b, sess)
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

func TestApplySessionConfigFromMetadata(t *testing.T) {
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
	seed, err := store.OpenSession(ctx, "/tmp/test", "openrouter/openai/gpt-5.4", "main")
	if err != nil {
		t.Fatalf("open seed session: %v", err)
	}
	seedID := seed.ID()
	if err := seed.Close(); err != nil {
		t.Fatalf("close seed session: %v", err)
	}

	cfg := &config.Config{Provider: "local-api", Model: "qwen3.6:27b", ReasoningEffort: "high"}
	if err := applySessionConfigFromMetadata(ctx, store, seedID, cfg); err != nil {
		t.Fatalf("apply session config: %v", err)
	}
	if cfg.Provider != "openrouter" || cfg.Model != "openai/gpt-5.4" {
		t.Fatalf("cfg provider/model = %s/%s, want openrouter/openai/gpt-5.4", cfg.Provider, cfg.Model)
	}
	if cfg.ReasoningEffort != "high" {
		t.Fatalf("reasoning effort = %q, want high", cfg.ReasoningEffort)
	}
}

func TestSplitSessionModelName(t *testing.T) {
	provider, model := splitSessionModelName("openrouter/openai/gpt-5.4")
	if provider != "openrouter" || model != "openai/gpt-5.4" {
		t.Fatalf("split openrouter model = %q/%q", provider, model)
	}
	provider, model = splitSessionModelName("claude-pro")
	if provider != "claude-pro" || model != "" {
		t.Fatalf("split subscription model = %q/%q", provider, model)
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
