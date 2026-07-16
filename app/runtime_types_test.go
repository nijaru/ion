package app

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
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
		),
	}
}

func completeRuntimeHandles(runner agent.Runner) Handles {
	return Handles{
		Backend: stubBackend{provider: "openai", model: "gpt-4.1"},
		Runner:  runner,
		Storage: newStubSession("runtime"),
	}
}

func TestSwitchRejectsIncompleteRuntimeWithoutPersisting(t *testing.T) {
	oldRunner := &trackingRuntimeRunner{}
	newRunner := &trackingRuntimeRunner{}
	input := runtimeSwitchInput(completeRuntimeHandles(oldRunner))
	saved := false
	input.Switcher = func(context.Context, *config.Config, string) (Backend, agent.Runner, session.Session, error) {
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
	input.Switcher = func(context.Context, *config.Config, string) (Backend, agent.Runner, session.Session, error) {
		handles := completeRuntimeHandles(newRunner)
		return handles.Backend, handles.Runner, handles.Storage.(session.Session), nil
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

func TestSwitchAcceptsCompleteRuntimeAfterPersistence(t *testing.T) {
	newRunner := &trackingRuntimeRunner{}
	input := runtimeSwitchInput(completeRuntimeHandles(&trackingRuntimeRunner{}))
	input.Switcher = func(context.Context, *config.Config, string) (Backend, agent.Runner, session.Session, error) {
		handles := completeRuntimeHandles(newRunner)
		return handles.Backend, handles.Runner, handles.Storage.(session.Session), nil
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

func TestResumeRejectsIncompleteRuntimeWithoutPersisting(t *testing.T) {
	oldRunner := &trackingRuntimeRunner{}
	newStorage := newStubSession("resume")
	cfg := &config.Config{Provider: "openai", Model: "gpt-4.1"}
	input := ResumeInput{
		Transition: NewTransition(cfg, cfg, PresetPrimary, ""),
		Current:    completeRuntimeHandles(oldRunner),
		SessionID:  "resume",
		Switcher: func(context.Context, *config.Config, string) (Backend, agent.Runner, session.Session, error) {
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
