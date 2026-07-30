package app

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
)

type trackingRuntimeRunner struct {
	stubRunner
	closed int
}

func (r *trackingRuntimeRunner) Close() error {
	r.closed++
	return nil
}

func runtimeSwitchInput(current Handles) SwitchInput {
	cfg := &config.Config{Provider: "openai", Model: "gpt-4.1"}
	return SwitchInput{
		Config:  cfg,
		Current: current,
		Transition: NewTransition(
			cfg,
			cfg,
			PresetPrimary,
			"",
		).WithStatePersistence(),
	}
}

func completeRuntimeHandles(runner agent.Runtime) Handles {
	return Handles{
		Info:    stubBackend{provider: "openai", model: "gpt-4.1"},
		Runner:  runner,
		Storage: newStubSession("runtime"),
	}
}

func TestSwitchRejectsIncompleteRuntimeWithoutPersisting(t *testing.T) {
	oldRunner := &trackingRuntimeRunner{}
	newRunner := &trackingRuntimeRunner{}
	input := runtimeSwitchInput(completeRuntimeHandles(oldRunner))
	saved := false
	input.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		return stubBackend{provider: "openai", model: "gpt-4.1"}, newRunner, nil, nil
	}
	input.SaveState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}

	_, err := Switch(context.Background(), input)
	if err == nil || !strings.Contains(err.Error(), "session storage is nil") {
		t.Fatalf("Switch error = %v, want incomplete storage error", err)
	}
	if saved {
		t.Fatal("failed switch persisted runtime state")
	}
	if newRunner.closed != 1 {
		t.Fatalf("incomplete replacement runner closed = %d, want 1", newRunner.closed)
	}
	if oldRunner.closed != 0 {
		t.Fatalf("current runner closed = %d, want 0", oldRunner.closed)
	}
}

func TestSwitchReturnsPersistenceFailureAndClosesReplacement(t *testing.T) {
	oldRunner := &trackingRuntimeRunner{}
	newRunner := &trackingRuntimeRunner{}
	input := runtimeSwitchInput(completeRuntimeHandles(oldRunner))
	input.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		handles := completeRuntimeHandles(newRunner)
		return handles.Info, handles.Runner, handles.Storage, nil
	}
	input.SaveState = func(config.RuntimeStateUpdate) error {
		return errors.New("state store unavailable")
	}

	_, err := Switch(context.Background(), input)
	if err == nil || !strings.Contains(err.Error(), "persist runtime state") {
		t.Fatalf("Switch error = %v, want persistence error", err)
	}
	if newRunner.closed != 1 {
		t.Fatalf("replacement runner closed = %d, want 1", newRunner.closed)
	}
	if oldRunner.closed != 0 {
		t.Fatalf("current runner closed = %d, want 0", oldRunner.closed)
	}
}

func TestSwitchRejectsReplacementValidationWithoutPersisting(t *testing.T) {
	oldRunner := &trackingRuntimeRunner{}
	newRunner := &trackingRuntimeRunner{}
	input := runtimeSwitchInput(completeRuntimeHandles(oldRunner))
	input.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		handles := completeRuntimeHandles(newRunner)
		return handles.Info, handles.Runner, handles.Storage, nil
	}
	validated := false
	input.ValidateReplacement = func(context.Context, Handles) error {
		validated = true
		return errors.New("selected leaf unavailable")
	}
	saved := false
	input.SaveState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}

	_, err := Switch(context.Background(), input)
	if err == nil || !strings.Contains(err.Error(), "validate replacement") {
		t.Fatalf("Switch error = %v, want replacement validation error", err)
	}
	if !validated {
		t.Fatal("replacement validator was not called")
	}
	if saved {
		t.Fatal("rejected replacement persisted runtime state")
	}
	if newRunner.closed != 1 {
		t.Fatalf("rejected replacement runner closed = %d, want 1", newRunner.closed)
	}
	if oldRunner.closed != 0 {
		t.Fatalf("current runner closed = %d, want 0", oldRunner.closed)
	}
}

func TestSwitchSkipsPersistenceWhenValidationCancels(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	newRunner := &trackingRuntimeRunner{}
	input := runtimeSwitchInput(completeRuntimeHandles(&trackingRuntimeRunner{}))
	input.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		handles := completeRuntimeHandles(newRunner)
		return handles.Info, handles.Runner, handles.Storage, nil
	}
	input.ValidateReplacement = func(context.Context, Handles) error {
		cancel()
		return nil
	}
	saved := false
	input.SaveState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}

	_, err := Switch(ctx, input)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Switch error = %v, want context cancellation", err)
	}
	if saved {
		t.Fatal("canceled replacement persisted runtime state")
	}
	if newRunner.closed != 1 {
		t.Fatalf("canceled replacement runner closed = %d, want 1", newRunner.closed)
	}
}

func TestSwitchAcceptsCompleteRuntimeAfterPersistence(t *testing.T) {
	newRunner := &trackingRuntimeRunner{}
	input := runtimeSwitchInput(completeRuntimeHandles(&trackingRuntimeRunner{}))
	input.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		handles := completeRuntimeHandles(newRunner)
		return handles.Info, handles.Runner, handles.Storage, nil
	}
	saved := false
	input.SaveState = func(update config.RuntimeStateUpdate) error {
		saved = update.Config != nil && update.Config.Model == "gpt-4.1"
		return nil
	}

	result, err := Switch(context.Background(), input)
	if err != nil {
		t.Fatalf("Switch() error = %v", err)
	}
	if !saved {
		t.Fatal("successful switch did not persist runtime state")
	}
	if result.Runtime.Handles.Runner != newRunner {
		t.Fatal("successful switch did not return replacement runner")
	}
}

func TestSwitchPersistsTransitionSelectionFlags(t *testing.T) {
	cfg := &config.Config{
		Provider:        "openai",
		Model:           "gpt-4.1",
		ReasoningEffort: "high",
	}
	input := SwitchInput{
		Config: cfg,
		Transition: NewTransition(cfg, cfg, PresetFast, "").
			WithStatePersistence().
			WithReasoningPersistence().
			WithActivePresetPersistence(),
		Switcher: func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
			runner := &trackingRuntimeRunner{}
			handles := completeRuntimeHandles(runner)
			return handles.Info, handles.Runner, handles.Storage, nil
		},
	}
	var got config.RuntimeStateUpdate
	input.SaveState = func(update config.RuntimeStateUpdate) error {
		got = update
		return nil
	}

	if _, err := Switch(context.Background(), input); err != nil {
		t.Fatalf("Switch() error = %v", err)
	}
	if got.Config == nil || got.Config.Model != cfg.Model || !got.PersistConfig {
		t.Fatalf("persisted config update = %#v, want configured persistence", got)
	}
	if !got.PersistActivePreset || got.ActivePreset != "fast" {
		t.Fatalf("active preset update = %#v, want fast persistence", got)
	}
	if !got.PersistReasoning || got.ReasoningPreset != "fast" || got.ReasoningEffort != "high" {
		t.Fatalf("reasoning update = %#v, want fast/high persistence", got)
	}
}

func TestResumePersistsTransitionSelectionFlags(t *testing.T) {
	cfg := &config.Config{
		Provider:        "openai",
		Model:           "gpt-4.1",
		ReasoningEffort: "medium",
	}
	input := ResumeInput{
		Transition: NewTransition(cfg, cfg, PresetPrimary, "").
			WithStatePersistence().
			WithReasoningPersistence().
			WithActivePresetPersistence(),
		SessionID: "resume",
		Switcher: func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
			runner := &trackingRuntimeRunner{}
			handles := completeRuntimeHandles(runner)
			return handles.Info, handles.Runner, handles.Storage, nil
		},
	}
	var got config.RuntimeStateUpdate
	input.SaveState = func(update config.RuntimeStateUpdate) error {
		got = update
		return nil
	}

	if _, err := Resume(context.Background(), input); err != nil {
		t.Fatalf("Resume() error = %v", err)
	}
	if got.Config == nil || got.Config.Model != cfg.Model || !got.PersistConfig {
		t.Fatalf("resumed config update = %#v, want configured persistence", got)
	}
	if !got.PersistActivePreset || got.ActivePreset != "primary" {
		t.Fatalf("resumed active preset update = %#v, want primary persistence", got)
	}
	if !got.PersistReasoning || got.ReasoningPreset != "primary" || got.ReasoningEffort != "medium" {
		t.Fatalf("resumed reasoning update = %#v, want primary/medium persistence", got)
	}
}

func TestResumeRejectsReplacementValidationWithoutPersisting(t *testing.T) {
	oldRunner := &trackingRuntimeRunner{}
	newRunner := &trackingRuntimeRunner{}
	newStorage := newStubSession("resume")
	cfg := &config.Config{Provider: "openai", Model: "gpt-4.1"}
	input := ResumeInput{
		Transition: NewTransition(cfg, cfg, PresetPrimary, "").WithStatePersistence(),
		Current:    completeRuntimeHandles(oldRunner),
		SessionID:  "resume",
		Switcher: func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
			handles := completeRuntimeHandles(newRunner)
			handles.Storage = newStorage
			return handles.Info, handles.Runner, handles.Storage, nil
		},
	}
	input.ValidateReplacement = func(context.Context, Handles) error {
		return errors.New("active projection unavailable")
	}
	saved := false
	input.SaveState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}

	_, err := Resume(context.Background(), input)
	if err == nil || !strings.Contains(err.Error(), "validate replacement") {
		t.Fatalf("Resume error = %v, want replacement validation error", err)
	}
	if saved {
		t.Fatal("rejected resumed replacement persisted runtime state")
	}
	if newRunner.closed != 1 {
		t.Fatalf("rejected resumed replacement runner closed = %d, want 1", newRunner.closed)
	}
	if oldRunner.closed != 0 {
		t.Fatalf("current runner closed = %d, want 0", oldRunner.closed)
	}
}

func TestResumeSkipsPersistenceWhenValidationCancels(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	oldRunner := &trackingRuntimeRunner{}
	newRunner := &trackingRuntimeRunner{}
	cfg := &config.Config{Provider: "openai", Model: "gpt-4.1"}
	input := ResumeInput{
		Transition: NewTransition(cfg, cfg, PresetPrimary, "").WithStatePersistence(),
		Current:    completeRuntimeHandles(oldRunner),
		SessionID:  "resume",
		Switcher: func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
			handles := completeRuntimeHandles(newRunner)
			return handles.Info, handles.Runner, handles.Storage, nil
		},
		ValidateReplacement: func(context.Context, Handles) error {
			cancel()
			return nil
		},
	}
	saved := false
	input.SaveState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}

	_, err := Resume(ctx, input)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Resume error = %v, want context cancellation", err)
	}
	if saved {
		t.Fatal("canceled resumed replacement persisted runtime state")
	}
	if newRunner.closed != 1 {
		t.Fatalf("canceled resumed replacement runner closed = %d, want 1", newRunner.closed)
	}
}

func TestResumeRejectsIncompleteRuntimeWithoutPersisting(t *testing.T) {
	oldRunner := &trackingRuntimeRunner{}
	newStorage := newStubSession("resume")
	cfg := &config.Config{Provider: "openai", Model: "gpt-4.1"}
	input := ResumeInput{
		Transition: NewTransition(cfg, cfg, PresetPrimary, ""),
		Current:    completeRuntimeHandles(oldRunner),
		SessionID:  "resume",
		Switcher: func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
			return stubBackend{provider: "openai", model: "gpt-4.1"}, nil, newStorage, nil
		},
	}
	saved := false
	input.SaveState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}

	_, err := Resume(context.Background(), input)
	if err == nil || !strings.Contains(err.Error(), "runner is nil") {
		t.Fatalf("Resume error = %v, want incomplete runner error", err)
	}
	if saved {
		t.Fatal("failed resume persisted runtime state")
	}
}
