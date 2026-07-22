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

func TestCurrentResumeLeafIDDoesNotUseStableSessionIdentity(t *testing.T) {
	model := readyModel(t)
	model.Model.Runtime = Snapshot{
		SessionID:    "stable-session-id",
		Materialized: true,
	}
	model.Model.LeafID = "conversation-leaf-id"

	if got := model.currentResumeLeafID(); got != "conversation-leaf-id" {
		t.Fatalf("resume leaf = %q, want conversation-leaf-id", got)
	}

	model.Model.LeafID = ""
	if got := model.currentResumeLeafID(); got != "" {
		t.Fatalf("resume leaf without a selected entry = %q, want empty", got)
	}
}

func TestDirectModelCommandRequiresIdle(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true

	_, cmd := model.handleCommand("/model model-b")
	if cmd == nil {
		t.Fatal("model command while a turn is active returned no guard")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "Finish or cancel the current turn") {
		t.Fatalf("error = %v, want busy-turn guard", err)
	}
}

type lookupSessionStore struct {
	resumeOnlyStore
	info session.SessionInfoEntry
}

func (s *lookupSessionStore) GetSessionInfo(context.Context, string) (session.SessionInfoEntry, error) {
	return s.info, nil
}

func TestStoredSessionConfigUsesDirectCatalogLookupForForeignWorkdir(t *testing.T) {
	model := readyModel(t)
	store := &lookupSessionStore{
		info: session.SessionInfoEntry{
			EntryBase: session.EntryBase{ID: "foreign-session"},
			Model:     "openai/gpt-4.1",
		},
	}

	cfg, err := model.storedSessionConfig(context.Background(), store, "foreign-session")
	if err != nil {
		t.Fatalf("storedSessionConfig() error = %v", err)
	}
	if cfg.Provider != "openai" || cfg.Model != "gpt-4.1" {
		t.Fatalf("stored session config = %s/%s, want openai/gpt-4.1", cfg.Provider, cfg.Model)
	}
}

func TestRuntimeSwitchUsesCancelableRequestContext(t *testing.T) {
	started := make(chan struct{})
	seen := make(chan context.Context, 1)
	switcher := func(ctx context.Context, _ *config.Config, _ string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		seen <- ctx
		close(started)
		<-ctx.Done()
		return nil, nil, nil, ctx.Err()
	}
	model := readyModel(t)
	model.Model.Switcher = switcher

	updated, cmd := model.switchRuntimeCommand(
		Transition{Snapshot: Snapshot{}},
		sysEntry("switch"),
		"",
		false,
	)
	if cmd == nil {
		t.Fatal("runtime switch returned no command")
	}
	requestContext := updated.runtimeRequestOperationContext()
	resultCh := make(chan any, 1)
	go func() { resultCh <- cmd() }()
	<-started
	if got := <-seen; got != requestContext {
		t.Fatalf("switch context = %v, want request context", got)
	}

	updated.rotateRuntimeContext()
	result, ok := (<-resultCh).(runtimeSwitchErrorMsg)
	if !ok || !errors.Is(result.err, context.Canceled) {
		t.Fatalf("canceled switch result = %#v, want context cancellation", result)
	}
}
