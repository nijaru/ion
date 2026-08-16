package main

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/nijaru/ion/config"
)

func TestConfiguredStartupModelIgnoresLastUsedPreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := os.MkdirAll(filepath.Join(home, ".ion"), 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(home, ".ion", "state.toml"),
		[]byte("active_preset = \"fast\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	cfg := &config.Config{
		Provider:           "openai",
		Model:              "gpt-4.1",
		FastModel:          "gpt-4.1-mini",
		StartupModelPolicy: "configured",
	}
	runtimeCfg, preset, err := startupRuntimeConfig(t.Context(), cfg, "", false)
	if err != nil {
		t.Fatalf("startupRuntimeConfig: %v", err)
	}
	if preset != "primary" || runtimeCfg.Model != "gpt-4.1" {
		t.Fatalf("startup selection = preset=%q model=%q, want primary/gpt-4.1", preset, runtimeCfg.Model)
	}
}

func TestResolveStartupConfigHonorsCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	cancel()

	err := resolveStartupConfig(ctx, &config.Config{
		Provider: "openai-compatible",
		Model:    "qwen",
	}, nil)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("resolveStartupConfig() error = %v, want context.Canceled", err)
	}
}
