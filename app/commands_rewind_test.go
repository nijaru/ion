package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
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
