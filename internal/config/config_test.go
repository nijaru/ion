package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestLoadReadsConfigFile(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}

	path := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(path, []byte(
		"provider = \"openai\"\nmodel = \"gpt-4o\"\nreasoning_effort = \"med\"\nfast_model = \"gpt-4.1-mini\"\nfast_reasoning_effort = \"low\"\nsummary_model = \"gpt-4o-mini\"\nsummary_reasoning_effort = \"low\"\nendpoint = \"https://example.com/v1\"\nauth_env_var = \"OPENAI_PROXY_KEY\"\ncontext_limit = 128000\nsession_retention_days = 14\n[extra_headers]\n\"X-Test\" = \"value\"\n",
	), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Provider != "openai" {
		t.Fatalf("provider = %q, want %q", cfg.Provider, "openai")
	}
	if cfg.Model != "gpt-4o" {
		t.Fatalf("model = %q, want %q", cfg.Model, "gpt-4o")
	}
	if cfg.ReasoningEffort != "medium" {
		t.Fatalf("reasoning_effort = %q, want %q", cfg.ReasoningEffort, "medium")
	}
	if cfg.FastModel != "gpt-4.1-mini" {
		t.Fatalf("fast_model = %q, want %q", cfg.FastModel, "gpt-4.1-mini")
	}
	if cfg.FastReasoningEffort != "low" {
		t.Fatalf("fast_reasoning_effort = %q, want %q", cfg.FastReasoningEffort, "low")
	}
	if cfg.SummaryModel != "gpt-4o-mini" {
		t.Fatalf("summary_model = %q, want %q", cfg.SummaryModel, "gpt-4o-mini")
	}
	if cfg.SummaryReasoningEffort != "low" {
		t.Fatalf("summary_reasoning_effort = %q, want %q", cfg.SummaryReasoningEffort, "low")
	}
	if cfg.Endpoint != "https://example.com/v1" {
		t.Fatalf("endpoint = %q, want %q", cfg.Endpoint, "https://example.com/v1")
	}
	if cfg.AuthEnvVar != "OPENAI_PROXY_KEY" {
		t.Fatalf("auth_env_var = %q, want %q", cfg.AuthEnvVar, "OPENAI_PROXY_KEY")
	}
	if got := cfg.ExtraHeaders["X-Test"]; got != "value" {
		t.Fatalf("extra_headers[X-Test] = %q, want %q", got, "value")
	}
	if cfg.ContextLimit != 128000 {
		t.Fatalf("context_limit = %d, want %d", cfg.ContextLimit, 128000)
	}
	if cfg.SessionRetentionDays != 14 {
		t.Fatalf("session_retention_days = %d, want %d", cfg.SessionRetentionDays, 14)
	}
}

func TestLoadUsesDefaultsWhenConfigMissing(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Provider != "" {
		t.Fatalf("provider = %q, want empty", cfg.Provider)
	}
	if cfg.Model != "" {
		t.Fatalf("model = %q, want empty", cfg.Model)
	}
	if cfg.ContextLimit != 0 {
		t.Fatalf("context_limit = %d, want %d", cfg.ContextLimit, 0)
	}
	if cfg.ReasoningEffort != DefaultReasoningEffort {
		t.Fatalf("reasoning_effort = %q, want %q", cfg.ReasoningEffort, DefaultReasoningEffort)
	}
	if cfg.SessionRetentionDays != DefaultSessionRetentionDays {
		t.Fatalf("session_retention_days = %d, want %d", cfg.SessionRetentionDays, DefaultSessionRetentionDays)
	}
}

func TestLoadAppliesEnvOverrides(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ION_MODEL", "openrouter openai/gpt-5.4")
	t.Setenv("ION_PROVIDER", "anthropic")
	t.Setenv("ION_REASONING_EFFORT", "high")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Provider != "anthropic" {
		t.Fatalf("provider = %q, want %q", cfg.Provider, "anthropic")
	}
	if cfg.Model != "openai/gpt-5.4" {
		t.Fatalf("model = %q, want %q", cfg.Model, "openai/gpt-5.4")
	}
	if cfg.ReasoningEffort != "high" {
		t.Fatalf("reasoning_effort = %q, want %q", cfg.ReasoningEffort, "high")
	}
}

func TestSaveWritesStatePath(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg := &Config{
		Provider:               "openai",
		Model:                  "gpt-4o",
		ReasoningEffort:        "low",
		FastModel:              "gpt-4.1-mini",
		FastReasoningEffort:    "low",
		SummaryModel:           "gpt-4o-mini",
		SummaryReasoningEffort: "low",
		Endpoint:               "https://example.com/v1",
		AuthEnvVar:             "OPENAI_PROXY_KEY",
		ExtraHeaders:           map[string]string{"X-Test": "value"},
		ContextLimit:           128000,
		SessionRetentionDays:   14,
	}
	if err := Save(cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	path := filepath.Join(home, ".ion", "config.toml")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read saved config: %v", err)
	}
	got := string(data)
	for _, want := range []string{
		`provider =`,
		`openai`,
		`model =`,
		`gpt-4o`,
		`fast_model =`,
		`gpt-4.1-mini`,
		`fast_reasoning_effort =`,
		`summary_model =`,
		`gpt-4o-mini`,
		`reasoning_effort = 'low'`,
		`endpoint = 'https://example.com/v1'`,
		`auth_env_var = 'OPENAI_PROXY_KEY'`,
		`[extra_headers]`,
		`X-Test = 'value'`,
		`context_limit = 128000`,
		`session_retention_days = 14`,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("saved config missing %q:\n%s", want, got)
		}
	}
}

func TestDefaultDataDirUsesIonDataDir(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	got, err := DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}
	want := filepath.Join(home, ".ion", "data")
	if got != want {
		t.Fatalf("data dir = %q, want %q", got, want)
	}
}

func TestDefaultModelCacheTTLSeconds(t *testing.T) {
	if got := DefaultModelCacheTTLSeconds(); got != 3600 {
		t.Fatalf("ttl = %d, want %d", got, 3600)
	}
}

func TestNormalizeReasoningEffort(t *testing.T) {
	for input, want := range map[string]string{
		"":       DefaultReasoningEffort,
		"auto":   DefaultReasoningEffort,
		"med":    "medium",
		"medium": "medium",
		"LOW":    "low",
		"weird":  DefaultReasoningEffort,
	} {
		if got := normalizeReasoningEffort(input); got != want {
			t.Fatalf("normalizeReasoningEffort(%q) = %q, want %q", input, got, want)
		}
	}
}
