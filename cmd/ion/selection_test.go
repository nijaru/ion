package main

import (
	"context"
	"flag"
	"os"
	"slices"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

// testStore implements session.Store for testing.
type testStore struct {
	entries  []session.Entry
	sessions []session.SessionInfoEntry
	listErr  error
	leafID   string
	meta     session.Metadata
}

func (s *testStore) Append(_ context.Context, _ session.Entry) (string, error) {
	return "", nil
}
func (s *testStore) AppendLeafEntry(ctx context.Context, entry session.Entry) (string, error) {
	return s.Append(ctx, entry)
}
func (s *testStore) AppendBatch(_ context.Context, _ []session.Entry) ([]string, error) {
	return nil, nil
}
func (s *testStore) GetEntry(_ context.Context, _ string) (session.Entry, error) {
	return nil, os.ErrNotExist
}
func (s *testStore) Branch(_ context.Context) ([]session.Entry, error) {
	return s.entries, nil
}
func (s *testStore) Entries(_ context.Context) ([]session.Entry, error) {
	return s.entries, nil
}
func (s *testStore) GetLeafID() string         { return s.leafID }
func (s *testStore) SetLeafID(id string) error { s.leafID = id; return nil }
func (s *testStore) Meta() session.Metadata    { return s.meta }
func (s *testStore) GetInputs(_ context.Context, _ string, _ int) ([]string, error) {
	return nil, nil
}
func (s *testStore) ListSessions(_ context.Context, _ string) ([]session.SessionInfoEntry, error) {
	if s.listErr != nil {
		return nil, s.listErr
	}
	return s.sessions, nil
}
func (s *testStore) UpdateSession(_ context.Context, _ session.SessionInfoEntry) error {
	return nil
}
func (s *testStore) AddInput(_ context.Context, _ string, _ string) error { return nil }
func (s *testStore) Close() error                                         { return nil }

func sessionInfoForTest(id, lastPreview string) session.SessionInfoEntry {
	return session.SessionInfoEntry{
		EntryBase:   session.EntryBase{ID: id},
		LastPreview: lastPreview,
	}
}

func TestNormalizeFlagArgsAcceptsLeadingSeparator(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"--", "--print", "hello"})
	want := []string{"--print", "hello"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsOpensPickerForResumeWithoutID(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"--resume"})
	if !openResumePicker {
		t.Fatal("normalizeFlagArgs did not open resume picker")
	}
	if len(got) != 0 {
		t.Fatalf("normalizeFlagArgs = %#v, want empty", got)
	}
}

func TestNormalizeFlagArgsKeepsModelAndThinkingValues(t *testing.T) {
	got, _ := normalizeFlagArgs([]string{"--model", "gpt-4.1"})
	want := []string{"--model", "gpt-4.1"}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}

	got, _ = normalizeFlagArgs([]string{"--thinking", "medium"})
	want = []string{"--thinking", "medium"}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsKeepsSessionPolicyFlags(t *testing.T) {
	got, _ := normalizeFlagArgs([]string{"--session", "abc123", "-p", "hello"})
	want := []string{"--session", "abc123", "-p", "hello"}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestSessionIDIsNotARecognizedCLIFlag(t *testing.T) {
	args, openResumePicker := normalizeFlagArgs([]string{"--session-id", "abc123"})
	if openResumePicker {
		t.Fatal("removed session-id flag opened the resume picker")
	}
	if want := []string{"--session-id", "abc123"}; !slices.Equal(args, want) {
		t.Fatalf("normalized removed flag = %#v, want %#v", args, want)
	}

	fs := flag.NewFlagSet("ion-test", flag.ContinueOnError)
	session := fs.String("session", "", "session")
	if err := fs.Parse(args); err == nil {
		t.Fatal("removed session-id flag was accepted by the CLI parser")
	}
	if *session != "" {
		t.Fatalf("removed flag changed session selection: %q", *session)
	}
}

func TestValidateAPIKeyOverride(t *testing.T) {
	if err := validateAPIKeyOverride("secret", ""); err == nil {
		t.Fatal("expected missing model error")
	}
	if err := validateAPIKeyOverride("secret", "gpt-test"); err != nil {
		t.Fatal(err)
	}
	if err := validateAPIKeyOverride("secret", "fast-model"); err != nil {
		t.Fatal(err)
	}
	if err := validateAPIKeyOverride("", ""); err != nil {
		t.Fatal(err)
	}
}

func TestCLIAPIKeyOverrideAccessor(t *testing.T) {
	value := " secret "
	flags := cliFlags{apiKeyFlag: &value}
	if got := flags.apiKeyOverride(); got != "secret" {
		t.Fatalf("apiKeyOverride = %q, want secret", got)
	}
}

func TestApplyCLIConfigOverrides(t *testing.T) {
	cfg := &config.Config{}

	t.Run("provider override", func(t *testing.T) {
		applyCLIConfigOverrides(cfg, "anthropic", "", "")
		if cfg.Provider != "anthropic" {
			t.Fatalf("provider = %q, want anthropic", cfg.Provider)
		}
	})

	t.Run("model override", func(t *testing.T) {
		applyCLIConfigOverrides(cfg, "", "claude-sonnet-4-20250514", "")
		if cfg.Model != "claude-sonnet-4-20250514" {
			t.Fatalf("model = %q, want claude-sonnet-4-20250514", cfg.Model)
		}
	})

	t.Run("thinking override", func(t *testing.T) {
		applyCLIConfigOverrides(cfg, "", "", "medium")
		if cfg.ReasoningEffort != "medium" {
			t.Fatalf("reasoning_effort = %q, want medium", cfg.ReasoningEffort)
		}
	})
}

func TestApplyCLITrustModeOverride(t *testing.T) {
	cfg := &config.Config{}
	applyCLITrustModeOverride(cfg, "confirm")
	if got := cfg.ToolTrustMode(); got != "confirm" {
		t.Fatalf("trust mode = %q, want confirm", got)
	}
	applyCLITrustModeOverride(cfg, "unknown")
	if got := cfg.ToolTrustMode(); got != "confirm" {
		t.Fatalf("unknown trust mode = %q, want fail-closed confirm", got)
	}
}

func TestLoadEffectiveConfigForReloadPreservesProcessOverrides(t *testing.T) {
	loaded := &config.Config{
		Provider:        "anthropic",
		Model:           "disk-model",
		TrustMode:       "confirm",
		ReasoningEffort: "low",
	}

	cfg, err := loadEffectiveConfigForReload(
		func() (*config.Config, error) { return loaded, nil },
		"openai",
		"cli-model",
		"high",
		"trusted",
		"cli-key",
	)
	if err != nil {
		t.Fatalf("loadEffectiveConfigForReload() error = %v", err)
	}
	if cfg.Provider != "openai" || cfg.Model != "cli-model" {
		t.Fatalf("provider/model = %q/%q, want openai/cli-model", cfg.Provider, cfg.Model)
	}
	if cfg.ReasoningEffort != "high" || cfg.ToolTrustMode() != "trusted" {
		t.Fatalf("process overrides = reasoning %q, trust %q", cfg.ReasoningEffort, cfg.ToolTrustMode())
	}
	if cfg.APIKeyOverride != "cli-key" || cfg.APIKeyOverrideProvider != "openai" {
		t.Fatalf("API key override = %q/%q, want cli-key/openai", cfg.APIKeyOverride, cfg.APIKeyOverrideProvider)
	}
}

func TestRecentSessionForContinueSkipsEmptyAndSlashOnlySessions(t *testing.T) {
	store := &testStore{sessions: []session.SessionInfoEntry{
		sessionInfoForTest("empty", ""),
		sessionInfoForTest("slash", "/resume"),
		{
			EntryBase:   session.EntryBase{ID: "slash-title"},
			Name:        "/model",
			LastPreview: "hi",
		},
		sessionInfoForTest("real", "hello"),
	}}

	recent, err := recentSessionForContinue(context.Background(), store, "/tmp/test")
	if err != nil {
		t.Fatalf("recent session: %v", err)
	}
	if recent == nil || recent.ID() != "real" {
		t.Fatalf("recent = %#v, want real", recent)
	}
}

func TestStartupSessionIDContinuesConversationSession(t *testing.T) {
	store := &testStore{sessions: []session.SessionInfoEntry{
		sessionInfoForTest("empty", ""),
		sessionInfoForTest("real", "hello"),
	}}

	id, err := startupSessionID(context.Background(), store, "/tmp/test", "", "", "", true)
	if err != nil {
		t.Fatalf("startupSessionID returned error: %v", err)
	}
	if id != "real" {
		t.Fatalf("session ID = %q, want real", id)
	}
}

func TestStartupSessionIDRejectsMissingContinueSession(t *testing.T) {
	store := &testStore{}

	id, err := startupSessionID(context.Background(), store, "/tmp/test", "", "", "", true)
	if err == nil || !strings.Contains(err.Error(), "no conversation session to continue") {
		t.Fatalf("startupSessionID id=%q error=%v, want missing continue error", id, err)
	}
}

func TestStartupSessionIDPropagatesContinueLookupError(t *testing.T) {
	store := &testStore{listErr: os.ErrPermission}

	id, err := startupSessionID(context.Background(), store, "/tmp/test", "", "", "", true)
	if err == nil || !strings.Contains(err.Error(), "failed to find recent session") {
		t.Fatalf("startupSessionID id=%q error=%v, want lookup error", id, err)
	}
}

func TestStartupSessionIDPrefersExplicitResume(t *testing.T) {
	store := &testStore{sessions: []session.SessionInfoEntry{
		sessionInfoForTest("recent", "hello"),
	}}

	id, err := startupSessionID(
		context.Background(),
		store,
		"/tmp/test",
		"session",
		"explicit",
		"",
		true,
	)
	if err != nil {
		t.Fatalf("startupSessionID returned error: %v", err)
	}
	if id != "session" {
		t.Fatalf("session ID = %q, want session", id)
	}

	id, err = startupSessionID(context.Background(), store, "/tmp/test", "", "explicit", "", true)
	if err != nil {
		t.Fatalf("startupSessionID returned error: %v", err)
	}
	if id != "explicit" {
		t.Fatalf("session ID = %q, want explicit", id)
	}

	id, err = startupSessionID(context.Background(), store, "/tmp/test", "", "", "short", true)
	if err != nil {
		t.Fatalf("startupSessionID short returned error: %v", err)
	}
	if id != "short" {
		t.Fatalf("session ID = %q, want short", id)
	}
}

func TestValidateForkSelectionRequiresAndExclusivelyUsesSessionSelection(t *testing.T) {
	if err := validateForkSelection(true, false, false, false, "", "", "", false, false, "", ""); err == nil {
		t.Fatal("fork without a source selection succeeded")
	}
	if err := validateForkSelection(true, true, false, false, "source", "", "", false, false, "", ""); err == nil {
		t.Fatal("fork with --no-session succeeded")
	}
	if err := validateForkSelection(true, false, true, false, "source", "", "", false, false, "", ""); err == nil {
		t.Fatal("fork with print mode succeeded")
	}
	if err := validateForkSelection(true, false, false, false, "source", "", "", false, false, "", ""); err != nil {
		t.Fatalf("valid fork selection rejected: %v", err)
	}
	if err := validateForkSelection(true, false, false, false, "", "", "", false, true, "", ""); err != nil {
		t.Fatalf("interactive fork selection rejected: %v", err)
	}
}

func TestSessionModelName(t *testing.T) {
	cases := []struct {
		provider, model, want string
	}{
		{"openai", "gpt-4.1", "openai/gpt-4.1"},
		{"anthropic", "claude-sonnet-4-20250514", "anthropic/claude-sonnet-4-20250514"},
		{"ollama", "llama3", "ollama/llama3"},
	}
	for _, c := range cases {
		if got := sessionModelName(c.provider, c.model); got != c.want {
			t.Fatalf("sessionModelName(%q, %q) = %q, want %q", c.provider, c.model, got, c.want)
		}
	}
}

func TestSplitSessionModelName(t *testing.T) {
	provider, model := splitSessionModelName("openai/gpt-4.1")
	if provider != "openai" || model != "gpt-4.1" {
		t.Fatalf("splitSessionModelName = (%q, %q), want (openai, gpt-4.1)", provider, model)
	}

	provider, model = splitSessionModelName("ollama/llama3")
	if provider != "ollama" || model != "llama3" {
		t.Fatalf("splitSessionModelName = (%q, %q), want (ollama, llama3)", provider, model)
	}
}

func TestBackendForProvider(t *testing.T) {
	for _, provider := range []string{"bad", "claude-pro"} {
		_, err := backendForProvider(provider)
		if err == nil || !strings.Contains(err.Error(), "unsupported provider") {
			t.Fatalf("provider %q error = %v, want unsupported provider", provider, err)
		}
	}
}
