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

type resumeProjectionRunner struct {
	*stubRunner
	projection    agent.SessionProjection
	projectionErr error
	closed        int
}

func (r *resumeProjectionRunner) SessionProjection(context.Context) (agent.SessionProjection, error) {
	return r.projection, r.projectionErr
}

func (r *resumeProjectionRunner) Close() error {
	r.closed++
	return nil
}

type resumeProjectionStorage struct {
	entries      []session.Entry
	entriesCalls int
	metaCalls    int
	branch       string
}

func (s *resumeProjectionStorage) Entries(context.Context) ([]session.Entry, error) {
	s.entriesCalls++
	return append([]session.Entry(nil), s.entries...), nil
}

func (s *resumeProjectionStorage) ID() string { return "resumed" }

func (s *resumeProjectionStorage) Meta() session.Metadata {
	s.metaCalls++
	return session.Metadata{ID: s.ID(), Branch: s.branch}
}

func (s *resumeProjectionStorage) Usage(context.Context) (session.Usage, error) {
	return session.Usage{}, nil
}

func TestResumeReplaysRuntimeOwnedBranchProjection(t *testing.T) {
	active := []session.Entry{sysEntry("active branch")}
	abandoned := []session.Entry{sysEntry("abandoned branch")}
	runner := &resumeProjectionRunner{
		stubRunner: &stubRunner{},
		projection: agent.SessionProjection{
			ID:             "resumed",
			LeafID:         active[0].ID(),
			Branch:         active,
			WorktreeBranch: "feature/resumed",
		},
	}
	storage := &resumeProjectionStorage{entries: abandoned, branch: "stale-storage-branch"}
	model := readyModel(t)
	model.Model.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		return stubBackend{}, runner, storage, nil
	}

	updated, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "test-model"},
		sysEntry("resume"),
		"resumed",
	)
	if cmd == nil {
		t.Fatal("resume command is nil")
	}
	msg, ok := cmd().(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("resume result = %T, want runtimeSwitchedMsg", cmd())
	}
	if storage.entriesCalls != 0 {
		t.Fatalf("storage Entries calls = %d, want 0", storage.entriesCalls)
	}
	if len(msg.replayEntries) != len(active) || session.EntryText(msg.replayEntries[0]) != "active branch" {
		t.Fatalf("replay entries = %#v, want active branch only", msg.replayEntries)
	}
	if msg.leafID != active[0].ID() {
		t.Fatalf("replay leaf = %q, want %q", msg.leafID, active[0].ID())
	}
	if !strings.Contains(strings.Join(msg.printLines, "\n"), "feature/resumed") {
		t.Fatalf("resume header = %q, want runtime-owned worktree branch", strings.Join(msg.printLines, "\n"))
	}
	if runner.closed != 0 {
		t.Fatalf("replacement runner close count = %d, want 0 before installation", runner.closed)
	}
	_ = updated
}

func TestResumeProjectionFailureClosesReplacement(t *testing.T) {
	projectionErr := errors.New("projection unavailable")
	previousSave := saveRuntimeState
	saved := false
	saveRuntimeState = func(config.RuntimeStateUpdate) error {
		saved = true
		return nil
	}
	defer func() { saveRuntimeState = previousSave }()
	runner := &resumeProjectionRunner{
		stubRunner:    &stubRunner{},
		projectionErr: projectionErr,
	}
	storage := &resumeProjectionStorage{}
	model := readyModel(t)
	model.Model.Switcher = func(context.Context, *config.Config, string) (RuntimeInfo, agent.Runtime, RuntimeStorage, error) {
		return stubBackend{}, runner, storage, nil
	}

	_, cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "test-model"},
		sysEntry("resume"),
		"resumed",
	)
	if cmd == nil {
		t.Fatal("resume command is nil")
	}
	msg, ok := cmd().(runtimeSwitchErrorMsg)
	if !ok {
		t.Fatalf("resume result = %T, want runtimeSwitchErrorMsg", cmd())
	}
	if !errors.Is(msg.err, projectionErr) {
		t.Fatalf("resume error = %v, want projection error", msg.err)
	}
	if runner.closed != 1 {
		t.Fatalf("replacement runner close count = %d, want 1", runner.closed)
	}
	if saved {
		t.Fatal("projection-rejected replacement persisted runtime state")
	}
	if storage.entriesCalls != 0 {
		t.Fatalf("storage Entries calls = %d, want 0", storage.entriesCalls)
	}
}

func TestRuntimeSwitchUsesAcceptedWorktreeBranch(t *testing.T) {
	model := readyModel(t)
	model.Model.RuntimeSwitchRequest = 1
	oldRunner := &stubRunner{}
	newRunner := &stubRunner{}
	storage := &resumeProjectionStorage{branch: "stale-storage-branch"}

	model.Model.Runner = oldRunner
	model, _ = model.handleRuntimeSwitched(runtimeSwitchedMsg{
		switchID:       1,
		runtime:        Accepted{Handles: Handles{Runner: newRunner, Storage: storage}},
		previous:       Handles{Runner: oldRunner},
		worktreeBranch: "accepted-runtime-branch",
	})
	if model.App.Branch != "accepted-runtime-branch" {
		t.Fatalf("installed worktree branch = %q, want accepted-runtime-branch", model.App.Branch)
	}
	if storage.metaCalls != 0 {
		t.Fatalf("storage Meta calls = %d, want 0 after runtime acceptance", storage.metaCalls)
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
