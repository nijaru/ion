package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

type projectionTestRunner struct {
	*stubRunner
	projection    agent.SessionProjection
	projectionCtx context.Context
	err           error
}

func (r *projectionTestRunner) SessionProjection(ctx context.Context) (agent.SessionProjection, error) {
	r.projectionCtx = ctx
	if err := ctx.Err(); err != nil {
		return agent.SessionProjection{}, err
	}
	return r.projection, r.err
}

type projectionTestStorage struct {
	entries  []session.Entry
	usage    session.Usage
	entryErr error
	usageErr error
	reads    int
}

func (s *projectionTestStorage) Entries(context.Context) ([]session.Entry, error) {
	s.reads++
	return s.entries, s.entryErr
}
func (s *projectionTestStorage) ID() string             { return "bootstrap-session" }
func (s *projectionTestStorage) Meta() session.Metadata { return session.Metadata{ID: s.ID()} }
func (s *projectionTestStorage) Usage(context.Context) (session.Usage, error) {
	s.reads++
	return s.usage, s.usageErr
}

func TestLoadSessionProjectionPrefersActiveRuntime(t *testing.T) {
	runtimeProjection := agent.SessionProjection{
		ID:     "runtime-session",
		LeafID: "runtime-leaf",
		Branch: []session.Entry{agentMsgEntry("active")},
		Usage:  session.Usage{Input: 11, Output: 7},
	}
	runner := &projectionTestRunner{stubRunner: &stubRunner{}, projection: runtimeProjection}
	storage := &projectionTestStorage{
		entries: []session.Entry{agentMsgEntry("bootstrap")},
		usage:   session.Usage{Input: 99, Output: 99},
	}

	got, err := loadSessionProjection(t.Context(), runner, storage)
	if err != nil {
		t.Fatalf("load active projection: %v", err)
	}
	if got.ID != runtimeProjection.ID || session.EntryText(got.Branch[0]) != "active" ||
		got.Usage != runtimeProjection.Usage {
		t.Fatalf("projection = %#v, want runtime projection %#v", got, runtimeProjection)
	}
	if storage.reads != 0 {
		t.Fatalf("bootstrap storage reads = %d, want zero with active runtime", storage.reads)
	}
}

func TestLoadSessionProjectionUsesStorageOnlyBeforeRuntime(t *testing.T) {
	storage := &projectionTestStorage{
		entries: []session.Entry{agentMsgEntry("bootstrap")},
		usage:   session.Usage{Input: 2, Output: 3},
	}

	got, err := loadSessionProjection(t.Context(), nil, storage)
	if err != nil {
		t.Fatalf("load bootstrap projection: %v", err)
	}
	if got.ID != storage.ID() || got.LeafID != got.Branch[0].ID() || got.Usage != storage.usage {
		t.Fatalf("projection = %#v, want bootstrap projection", got)
	}
	if storage.reads != 2 {
		t.Fatalf("bootstrap storage reads = %d, want entries and usage", storage.reads)
	}
}

func TestLoadSessionProjectionRejectsRuntimeWithoutCapability(t *testing.T) {
	_, err := loadSessionProjection(t.Context(), &stubRunner{}, &projectionTestStorage{})
	if err == nil || err.Error() != "active runtime does not support session projection" {
		t.Fatalf("error = %v, want missing projection capability", err)
	}
}

func TestActiveSessionCommandsUseRuntimeProjection(t *testing.T) {
	runtimeProjection := agent.SessionProjection{
		ID: "runtime-session",
		Branch: []session.Entry{
			&session.MessageEntry{Message: session.NewUserText("question", time.Time{})},
			agentMsgEntry("answer"),
		},
		Usage: session.Usage{Input: 8, Output: 4, Cost: session.Cost{Total: 0.4}},
	}
	runner := &projectionTestRunner{stubRunner: &stubRunner{}, projection: runtimeProjection}
	storage := &projectionTestStorage{entryErr: errors.New("active commands must not read storage")}
	model := readyModel(t)
	model.Model.Runner = runner
	model.Model.Storage = storage

	notice, err := model.sessionInfoNotice(context.Background())
	if err != nil {
		t.Fatalf("session info notice: %v", err)
	}
	for _, want := range []string{
		"id: runtime-session",
		"messages: user 1, assistant 1, tools 0, total 2",
		"tokens: input 8, output 4, total 12",
		"cost: $0.400000",
	} {
		if !strings.Contains(notice, want) {
			t.Fatalf("session notice = %q, missing %q", notice, want)
		}
	}

	usageCtx := context.Background()
	usageMsg, ok := loadSessionUsageCmd(usageCtx, 7, 3, runner, storage)().(sessionUsageLoadedMsg)
	if !ok {
		t.Fatalf(
			"usage message type = %T, want sessionUsageLoadedMsg",
			loadSessionUsageCmd(usageCtx, 7, 3, runner, storage)(),
		)
	}
	if usageMsg.err != nil || usageMsg.generation != 7 || usageMsg.treeNavigationRequest != 3 ||
		usageMsg.input != 8 || usageMsg.output != 4 || usageMsg.cost != 0.4 {
		t.Fatalf("usage message = %#v, want runtime projection usage", usageMsg)
	}
	if runner.projectionCtx != usageCtx {
		t.Fatalf("usage projection context = %v, want supplied context", runner.projectionCtx)
	}

	costMsg, ok := model.sessionCostCmd()().(sessionCostMsg)
	if !ok || costMsg.generation != model.Model.EventGeneration ||
		!strings.Contains(costMsg.notice, "cost: $0.400000") {
		t.Fatalf("cost message = %#v, want runtime projection cost", model.sessionCostCmd()())
	}
	if storage.reads != 0 {
		t.Fatalf("active command storage reads = %d, want zero", storage.reads)
	}
}

func TestLoadSessionUsageStopsOnCanceledRuntimeContext(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	runner := &projectionTestRunner{stubRunner: &stubRunner{}}

	result, ok := loadSessionUsageCmd(ctx, 4, 0, runner, nil)().(sessionUsageLoadedMsg)
	if !ok {
		t.Fatalf("usage result type = %T, want sessionUsageLoadedMsg", loadSessionUsageCmd(ctx, 4, 0, runner, nil)())
	}
	if result.generation != 4 || result.err == nil {
		t.Fatalf("canceled usage result = %#v, want generation and cancellation error", result)
	}
	if runner.projectionCtx != ctx {
		t.Fatalf("canceled usage context = %v, want supplied context", runner.projectionCtx)
	}
}
