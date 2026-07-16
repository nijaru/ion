package main

import (
	"os"
	"path/filepath"
	"testing"

	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

func TestTUICheckpointControllerFiltersAndRestoresCurrentWorkspace(t *testing.T) {
	workspacePath := t.TempDir()
	checkpointPath := filepath.Join(t.TempDir(), "checkpoints")
	store := ionworkspace.NewCheckpointStore(checkpointPath)
	file := filepath.Join(workspacePath, "main.go")
	if err := os.WriteFile(file, []byte("before\n"), 0o644); err != nil {
		t.Fatalf("write initial file: %v", err)
	}
	cp, err := store.Create(t.Context(), workspacePath, []string{"main.go"})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if err := os.WriteFile(file, []byte("after\n"), 0o644); err != nil {
		t.Fatalf("write changed file: %v", err)
	}

	controller := tuiCheckpointController{path: checkpointPath, workspace: workspacePath}
	items, err := controller.List(t.Context(), 10)
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(items) != 1 || items[0].ID != cp.ID || items[0].PathCount != 1 {
		t.Fatalf("items = %#v", items)
	}

	plan, err := controller.Plan(t.Context(), cp.ID)
	if err != nil {
		t.Fatalf("Plan: %v", err)
	}
	if len(plan.Conflicts) != 1 || plan.Conflicts[0].Path != "main.go" {
		t.Fatalf("plan = %#v", plan)
	}

	report, err := controller.Restore(t.Context(), cp.ID)
	if err != nil {
		t.Fatalf("Restore: %v", err)
	}
	if len(report.Restored) != 1 || report.Restored[0] != "main.go" {
		t.Fatalf("report = %#v", report)
	}
	data, err := os.ReadFile(file)
	if err != nil {
		t.Fatalf("read restored file: %v", err)
	}
	if string(data) != "before\n" {
		t.Fatalf("restored content = %q", data)
	}
}

func TestTUICheckpointControllerRejectsForeignWorkspace(t *testing.T) {
	workspacePath := t.TempDir()
	otherWorkspace := t.TempDir()
	checkpointPath := filepath.Join(t.TempDir(), "checkpoints")
	store := ionworkspace.NewCheckpointStore(checkpointPath)
	cp, err := store.Create(t.Context(), workspacePath, []string{"missing.txt"})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}

	controller := tuiCheckpointController{path: checkpointPath, workspace: otherWorkspace}
	if _, err := controller.Plan(t.Context(), cp.ID); err == nil {
		t.Fatal("Plan accepted a foreign workspace checkpoint")
	}
}
