package main

import (
	"context"
	"testing"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/storage"
)

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

	t.Run("subscription provider keeps model optional", func(t *testing.T) {
		cfg := &config.Config{Provider: "claude-pro"}
		if err := resolveStartupConfig(cfg); err != nil {
			t.Fatalf("resolveStartupConfig returned error: %v", err)
		}
		if cfg.Model != "" {
			t.Fatalf("model = %q, want empty", cfg.Model)
		}
	})

	t.Run("api provider requires model", func(t *testing.T) {
		cfg := &config.Config{Provider: "anthropic"}
		if err := resolveStartupConfig(cfg); err == nil {
			t.Fatal("resolveStartupConfig returned nil error for api provider without model")
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

	if _, ok := <-b.Session().Events(); !ok {
		t.Fatal("expected unconfigured session event channel to stay open")
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
