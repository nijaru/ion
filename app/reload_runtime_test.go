package app

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
)

func TestReloadMaterializesRuntimeAndKeybindingsTogether(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	if err := config.Save(&config.Config{Provider: "openai", Model: "reloaded-model"}); err != nil {
		t.Fatalf("save config: %v", err)
	}
	if err := SaveKeybindings(map[KeybindingAction]string{
		ActionCycleModelForward: "ctrl+x",
	}); err != nil {
		t.Fatalf("save keybindings: %v", err)
	}

	oldRunner := &trackingRuntimeRunner{}
	var observedModel string
	switcher := func(_ context.Context, cfg *config.Config, sessionID string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		observedModel = cfg.Model
		newSession := newStubSession(sessionID)
		return stubBackend{sess: newSession, provider: cfg.Provider, model: cfg.Model}, &stubRunner{}, newSession, nil
	}
	model := New(
		stubBackend{provider: "openai", model: "old-model"},
		nil,
		nil,
		"/tmp/ion-test",
		"main",
		"dev",
		switcher,
	)
	model.Model.Runner = oldRunner
	oldConfig := &config.Config{Provider: "openai", Model: "old-model"}
	model = model.WithConfigForRuntimePreset(oldConfig, oldConfig, "primary")
	oldKeybindings := model.Keybindings

	started, loadCmd := model.handleCommand("/reload")
	if loadCmd == nil {
		t.Fatal("reload returned no load command")
	}
	loaded := loadCmd()
	loadedMsg, ok := loaded.(reloadConfigLoadedMsg)
	if !ok {
		t.Fatalf("reload load message = %T, want reloadConfigLoadedMsg", loaded)
	}
	updated, switchCmd := started.Update(loadedMsg)
	model = testModel(t, updated)
	if model.Model.RuntimeSwitchRequest == 0 {
		t.Fatal("reload did not start runtime materialization")
	}

	if switchCmd == nil {
		t.Fatal("reload materialization returned no command")
	}
	// The handler returned the switch command through Update; execute it to
	// deliver the accepted runtime on the TUI event loop.
	switched := switchCmd()
	updated, _ = model.Update(switched)
	model = testModel(t, updated)

	if observedModel != "reloaded-model" {
		t.Fatalf("switcher model = %q, want reloaded-model", observedModel)
	}
	if model.Model.Config == nil || model.Model.Config.Model != "reloaded-model" {
		t.Fatalf("live config = %#v, want reloaded model", model.Model.Config)
	}
	if model.Model.Runtime.Model != "reloaded-model" {
		t.Fatalf("runtime model = %q, want reloaded-model", model.Model.Runtime.Model)
	}
	if model.Keybindings == oldKeybindings {
		t.Fatal("reload retained the old keybindings manager")
	}
	if got := model.Keybindings.ActionForKey("ctrl+x"); got != ActionCycleModelForward {
		t.Fatalf("reloaded keybinding action = %q, want %q", got, ActionCycleModelForward)
	}
	if oldRunner.closed != 1 {
		t.Fatalf("old runner close count = %d, want 1", oldRunner.closed)
	}
}

func TestReloadMaterializationFailureLeavesRuntimeAndKeybindingsUnchanged(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	if err := config.Save(&config.Config{Provider: "openai", Model: "reloaded-model"}); err != nil {
		t.Fatalf("save config: %v", err)
	}
	if err := SaveKeybindings(map[KeybindingAction]string{
		ActionCycleModelForward: "ctrl+x",
	}); err != nil {
		t.Fatalf("save keybindings: %v", err)
	}

	var switchCalls int
	switcher := func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		switchCalls++
		return nil, nil, nil, errors.New("provider unavailable")
	}
	model := New(
		stubBackend{provider: "openai", model: "old-model"},
		nil,
		nil,
		"/tmp/ion-test",
		"main",
		"dev",
		switcher,
	)
	model.Model.Runner = &stubRunner{}
	oldConfig := &config.Config{Provider: "openai", Model: "old-model"}
	model = model.WithConfigForRuntimePreset(oldConfig, oldConfig, "primary")
	oldKeybindings := model.Keybindings

	started, loadCmd := model.handleCommand("/reload")
	updated, switchCmd := started.Update(loadCmd())
	model = testModel(t, updated)
	if switchCmd == nil {
		t.Fatal("reload materialization returned no command")
	}
	updated, _ = model.Update(switchCmd())
	model = testModel(t, updated)

	if switchCalls != 1 {
		t.Fatalf("switcher calls = %d, want 1", switchCalls)
	}
	if model.Model.Config == nil || model.Model.Config.Model != "old-model" {
		t.Fatalf("failed reload changed config = %#v", model.Model.Config)
	}
	if model.Model.Runtime.Model != "old-model" {
		t.Fatalf("failed reload changed runtime model = %q", model.Model.Runtime.Model)
	}
	if model.Keybindings != oldKeybindings {
		t.Fatal("failed reload changed keybindings")
	}
	if model.Model.RuntimeSwitchRequest != 0 {
		t.Fatalf("runtime request = %d, want cleared", model.Model.RuntimeSwitchRequest)
	}
}

func TestReloadConfigParseFailureLeavesRuntimeUnchanged(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	path := filepath.Join(home, ".ion", "config.toml")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("provider = ["), 0o644); err != nil {
		t.Fatal(err)
	}

	model := New(stubBackend{provider: "openai", model: "old-model"}, nil, nil, "/tmp/ion-test", "main", "dev", nil)
	oldConfig := &config.Config{Provider: "openai", Model: "old-model"}
	model = model.WithConfigForRuntimePreset(oldConfig, oldConfig, "primary")
	oldKeybindings := model.Keybindings

	started, loadCmd := model.handleCommand("/reload")
	updated, resultCmd := started.Update(loadCmd())
	model = testModel(t, updated)
	_ = resultCmd
	if model.Model.Config == nil || model.Model.Config.Model != "old-model" {
		t.Fatalf("parse failure changed config = %#v", model.Model.Config)
	}
	if model.Keybindings != oldKeybindings {
		t.Fatal("parse failure changed keybindings")
	}
	if model.Model.RuntimeSwitchRequest != 0 {
		t.Fatalf("runtime request = %d, want cleared", model.Model.RuntimeSwitchRequest)
	}
}
