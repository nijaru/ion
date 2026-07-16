package main

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

func TestOpenRuntimeReturnsActionableProviderError(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "")
	store, err := session.NewSQLiteStore(":memory:", "canto")
	if err != nil {
		t.Fatalf("new sqlite store: %v", err)
	}
	defer store.Close()

	b, sess, runner, err := openRuntime(
		context.Background(),
		store,
		nil,
		"/tmp/ion-test",
		"main",
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		"target-session",
		false,
		"",
		"",
	)
	if err == nil || !strings.Contains(err.Error(), "OPENAI_API_KEY not set") {
		t.Fatalf("openRuntime error = %v, want actionable credential error", err)
	}
	if b == nil || b.Name() != "setup" {
		t.Fatalf("backend = %#v, want setup backend", b)
	}
	if sess != nil || runner != nil {
		t.Fatalf("incomplete runtime handles = (%v, %v), want nil", sess, runner)
	}
	if status := b.Bootstrap().Status; status != "OPENAI_API_KEY not set" {
		t.Fatalf("setup status = %q, want original provider error", status)
	}
	if leaf := store.GetLeafID(); leaf != "" {
		t.Fatalf("failed provider initialization moved store leaf to %q", leaf)
	}
}

func TestStartupSetupRequiredRecognizesSetupBackend(t *testing.T) {
	if !startupSetupRequired(app.NewSetupBackend(&config.Config{Provider: "openai"}, nil, "missing")) {
		t.Fatal("setup backend should require startup setup")
	}
	if startupSetupRequired(providerBackend{provider: "openai"}) {
		t.Fatal("materialized backend should not require startup setup")
	}
}
