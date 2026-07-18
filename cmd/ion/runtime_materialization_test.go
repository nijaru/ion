package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

func TestOpenRuntimeReturnsActionableProviderError(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "")
	store, err := session.NewSQLiteStore(":memory:", "ion")
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
		t.Fatalf("runtime info = %#v, want setup runtime", b)
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

func TestOpenRuntimeDoesNotMoveLeafWhenMaterializationFails(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatalf("new sqlite store: %v", err)
	}
	defer store.Close()

	sess := session.NewSession(store, 8)
	oldID, err := sess.AppendMessage(context.Background(), session.NewUserText("old", time.Now()))
	if err != nil {
		t.Fatalf("append old entry: %v", err)
	}
	targetID, err := sess.AppendMessage(context.Background(), session.NewUserText("target", time.Now()))
	if err != nil {
		t.Fatalf("append target entry: %v", err)
	}
	if err := store.SetLeafID(oldID); err != nil {
		t.Fatalf("restore old leaf: %v", err)
	}

	promptDir := filepath.Join(t.TempDir(), "prompt")
	if err := os.Mkdir(promptDir, 0o755); err != nil {
		t.Fatalf("mkdir prompt path: %v", err)
	}
	_, _, _, err = openRuntime(
		context.Background(),
		store,
		nil,
		"/tmp/ion-test",
		"main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		targetID,
		false,
		promptDir,
		"",
	)
	if err == nil || !strings.Contains(err.Error(), "build system prompt") {
		t.Fatalf("openRuntime error = %v, want system prompt failure", err)
	}
	if leaf := store.GetLeafID(); leaf != oldID {
		t.Fatalf("failed materialization moved leaf to %q, want %q", leaf, oldID)
	}
}

func TestStartupSetupRequiredRecognizesSetupBackend(t *testing.T) {
	if !startupSetupRequired(app.NewSetupRuntime(&config.Config{Provider: "openai"}, nil, "missing")) {
		t.Fatal("setup backend should require startup setup")
	}
	if startupSetupRequired(providerRuntimeInfo{provider: "openai"}) {
		t.Fatal("materialized backend should not require startup setup")
	}
}
