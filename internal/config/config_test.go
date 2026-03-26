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

	configDir := filepath.Join(home, ".config", "ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}

	path := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(path, []byte(
		"provider = \"openai\"\nmodel = \"gpt-4o\"\ncontext_limit = 128000\n",
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
	if cfg.ContextLimit != 128000 {
		t.Fatalf("context_limit = %d, want %d", cfg.ContextLimit, 128000)
	}
}

func TestLoadFallsBackToLegacyConfigFile(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	legacyDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(legacyDir, 0o755); err != nil {
		t.Fatalf("mkdir legacy dir: %v", err)
	}

	path := filepath.Join(legacyDir, "config.toml")
	if err := os.WriteFile(path, []byte(
		"provider = \"openrouter\"\nmodel = \"openai/gpt-5.4\"\ncontext_limit = 200000\n",
	), 0o644); err != nil {
		t.Fatalf("write legacy config: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Provider != "openrouter" {
		t.Fatalf("provider = %q, want %q", cfg.Provider, "openrouter")
	}
	if cfg.Model != "openai/gpt-5.4" {
		t.Fatalf("model = %q, want %q", cfg.Model, "openai/gpt-5.4")
	}
	if cfg.ContextLimit != 200000 {
		t.Fatalf("context_limit = %d, want %d", cfg.ContextLimit, 200000)
	}
}

func TestLoadAppliesEnvOverrides(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ION_MODEL", "openrouter openai/gpt-5.4")
	t.Setenv("ION_PROVIDER", "anthropic")

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
}

func TestSaveWritesUserConfigPath(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg := &Config{
		Provider:     "openai",
		Model:        "gpt-4o",
		ContextLimit: 128000,
	}
	if err := Save(cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	path := filepath.Join(home, ".config", "ion", "config.toml")
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
		`context_limit = 128000`,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("saved config missing %q:\n%s", want, got)
		}
	}
}

func TestLoadStateReadsInternalConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	stateDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(stateDir, 0o755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	path := filepath.Join(stateDir, "state.toml")
	if err := os.WriteFile(path, []byte(
		"data_dir = \"/tmp/ion\"\nmodel_cache_ttl_secs = 600\nsession_retention_days = 14\n",
	), 0o644); err != nil {
		t.Fatalf("write state: %v", err)
	}

	state, err := LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}

	if state.DataDir != "/tmp/ion" {
		t.Fatalf("data_dir = %q, want %q", state.DataDir, "/tmp/ion")
	}
	if state.ModelCacheTTLSeconds != 600 {
		t.Fatalf("model_cache_ttl_secs = %d, want %d", state.ModelCacheTTLSeconds, 600)
	}
	if state.SessionRetentionDays != 14 {
		t.Fatalf("session_retention_days = %d, want %d", state.SessionRetentionDays, 14)
	}
}

func TestSaveStateWritesInternalPath(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	state := &State{
		DataDir:              filepath.Join(home, ".ion"),
		ModelCacheTTLSeconds: 600,
		SessionRetentionDays: 14,
	}
	if err := SaveState(state); err != nil {
		t.Fatalf("save state: %v", err)
	}

	path := filepath.Join(home, ".ion", "state.toml")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read saved state: %v", err)
	}
	got := string(data)
	for _, want := range []string{
		`data_dir =`,
		`model_cache_ttl_secs = 600`,
		`session_retention_days = 14`,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("saved state missing %q:\n%s", want, got)
		}
	}
}
