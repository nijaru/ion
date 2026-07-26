package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

type stubCheckpointController struct {
	items    []CheckpointInfo
	plan     CheckpointPlan
	report   CheckpointReport
	listed   []int
	planned  []string
	restored []string
	err      error
}

type cancelAwareCheckpointController struct {
	restoreStarted chan struct{}
	restoreContext context.Context
}

func (c *cancelAwareCheckpointController) List(context.Context, int) ([]CheckpointInfo, error) {
	return nil, nil
}

func (c *cancelAwareCheckpointController) Plan(context.Context, string) (CheckpointPlan, error) {
	return CheckpointPlan{}, nil
}

func (c *cancelAwareCheckpointController) Restore(ctx context.Context, _ string) (CheckpointReport, error) {
	c.restoreContext = ctx
	close(c.restoreStarted)
	<-ctx.Done()
	return CheckpointReport{}, ctx.Err()
}

func (s *stubCheckpointController) List(_ context.Context, limit int) ([]CheckpointInfo, error) {
	s.listed = append(s.listed, limit)
	return append([]CheckpointInfo(nil), s.items...), s.err
}

func (s *stubCheckpointController) Plan(_ context.Context, id string) (CheckpointPlan, error) {
	s.planned = append(s.planned, id)
	plan := s.plan
	if plan.ID == "" {
		plan.ID = id
	}
	return plan, s.err
}

func (s *stubCheckpointController) Restore(_ context.Context, id string) (CheckpointReport, error) {
	s.restored = append(s.restored, id)
	return s.report, s.err
}

func TestRewindCommandListsPreviewsAndAppliesExplicitly(t *testing.T) {
	controller := &stubCheckpointController{
		items: []CheckpointInfo{{
			ID:        "cp-1",
			CreatedAt: time.Date(2026, 7, 16, 1, 2, 3, 0, time.UTC),
			PathCount: 2,
		}},
		plan: CheckpointPlan{
			ID: "cp-1",
			Conflicts: []CheckpointConflict{{
				Path:   "internal/app.go",
				Action: "overwrite",
			}},
		},
		report: CheckpointReport{Restored: []string{"internal/app.go"}},
	}
	model := readyModel(t).WithCheckpoints(controller)

	model, cmd := model.handleCommand("/rewind")
	if cmd == nil || model.Model.CheckpointRequest != 1 {
		t.Fatalf("list command = %v, request = %d", cmd, model.Model.CheckpointRequest)
	}
	if result, ok := cmd().(checkpointListMsg); !ok || len(result.items) != 1 {
		t.Fatalf("list result = %#v", result)
	}

	model, cmd = model.handleCommand("/rewind cp-1")
	if cmd == nil {
		t.Fatal("preview command returned nil")
	}
	if result, ok := cmd().(checkpointPlanMsg); !ok || result.plan.ID != "cp-1" {
		t.Fatalf("preview result = %#v", result)
	}
	if len(controller.planned) != 1 || controller.planned[0] != "cp-1" {
		t.Fatalf("planned = %#v", controller.planned)
	}

	model, cmd = model.handleCommand("/rewind cp-1 --apply")
	if cmd == nil {
		t.Fatal("apply command returned nil")
	}
	if result, ok := cmd().(checkpointRestoredMsg); !ok || result.id != "cp-1" {
		t.Fatalf("apply result = %#v", result)
	}
	if len(controller.restored) != 1 || controller.restored[0] != "cp-1" {
		t.Fatalf("restored = %#v", controller.restored)
	}
}

func TestRewindCommandRequiresExplicitApplyAndValidUsage(t *testing.T) {
	model := readyModel(t)
	_, cmd := model.handleCommand("/rewind")
	if cmd == nil {
		t.Fatal("unconfigured rewind returned no error")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "workspace checkpoints are unavailable" {
		t.Fatalf("unconfigured rewind error = %v", err)
	}

	controller := &stubCheckpointController{}
	model = model.WithCheckpoints(controller)
	_, cmd = model.handleCommand("/rewind cp-1 --bad")
	if cmd == nil {
		t.Fatal("invalid rewind usage returned no error")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "usage: /rewind [checkpoint-id [--apply]]" {
		t.Fatalf("usage error = %v", err)
	}

	controller.err = errors.New("checkpoint unavailable")
	model, cmd = model.handleCommand("/rewind cp-1")
	updated, resultCmd := model.update(cmd())
	_ = updated
	if err := localErrorFromMsg(t, resultCmd()); err == nil || err.Error() != "checkpoint unavailable" {
		t.Fatalf("backend error = %v", err)
	}
}

func TestFormatCheckpointOutputEscapesControlsAndListsChanges(t *testing.T) {
	list := formatCheckpointList([]CheckpointInfo{{ID: "cp\x1b[31m", PathCount: 1}})
	if strings.Contains(list, "\x1b") || !strings.Contains(list, `\u001b`) {
		t.Fatalf("checkpoint list contains raw terminal controls: %q", list)
	}
	plan := formatCheckpointPlan(CheckpointPlan{
		ID:        "cp-1",
		Conflicts: []CheckpointConflict{{Path: "dir/file.txt", Action: "remove"}},
	})
	if !strings.Contains(plan, "remove  dir/file.txt") || !strings.Contains(plan, "--apply") {
		t.Fatalf("checkpoint plan = %q", plan)
	}
}

func TestRewindIgnoresStaleAsyncResults(t *testing.T) {
	controller := &stubCheckpointController{}
	model := readyModel(t).WithCheckpoints(controller)
	model, first := model.handleCommand("/rewind")
	model, second := model.handleCommand("/rewind")
	if first == nil || second == nil {
		t.Fatal("rewind list commands missing")
	}
	updated, cmd := model.update(checkpointListMsg{requestID: 1})
	if cmd != nil {
		t.Fatal("stale checkpoint result returned a command")
	}
	if updated.Model.CheckpointRequest != 2 {
		t.Fatalf("checkpoint request = %d, want 2", updated.Model.CheckpointRequest)
	}
}

func TestRewindRestoreUsesCancelableRuntimeContext(t *testing.T) {
	controller := &cancelAwareCheckpointController{restoreStarted: make(chan struct{})}
	model := readyModel(t).WithCheckpoints(controller)
	model, cmd := model.handleCommand("/rewind cp-1 --apply")
	if cmd == nil {
		t.Fatal("restore command returned nil")
	}
	expectedContext := model.runtimeOperationContext()
	resultCh := make(chan tea.Msg, 1)
	go func() { resultCh <- cmd() }()
	select {
	case <-controller.restoreStarted:
	case <-time.After(time.Second):
		t.Fatal("restore did not start")
	}
	if controller.restoreContext != expectedContext {
		t.Fatalf("restore context = %v, want accepted runtime context", controller.restoreContext)
	}

	model.rotateRuntimeContext()
	model.Model.EventGeneration++
	result, ok := (<-resultCh).(checkpointRestoredMsg)
	if !ok || !errors.Is(result.err, context.Canceled) {
		t.Fatalf("canceled restore result = %#v, want context cancellation", result)
	}
	if result.generation == model.Model.EventGeneration {
		t.Fatalf("canceled restore retained current generation %d", result.generation)
	}
	updated, next := model.update(result)
	if next != nil {
		t.Fatal("stale canceled restore returned a command")
	}
	if updated.Model.CheckpointRequest != model.Model.CheckpointRequest {
		t.Fatalf(
			"stale restore changed checkpoint request: got %d want %d",
			updated.Model.CheckpointRequest,
			model.Model.CheckpointRequest,
		)
	}
}
