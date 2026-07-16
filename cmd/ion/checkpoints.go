package main

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/app"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

type tuiCheckpointController struct {
	path      string
	workspace string
}

func (c tuiCheckpointController) open() (*ionworkspace.CheckpointStore, error) {
	if c.path == "" {
		return nil, fmt.Errorf("checkpoint store path is not configured")
	}
	return ionworkspace.NewCheckpointStore(c.path), nil
}

func (c tuiCheckpointController) List(ctx context.Context, limit int) ([]app.CheckpointInfo, error) {
	store, err := c.open()
	if err != nil {
		return nil, err
	}
	items, err := store.List(ctx, c.workspace, limit)
	if err != nil {
		return nil, err
	}
	result := make([]app.CheckpointInfo, 0, len(items))
	for _, item := range items {
		result = append(result, app.CheckpointInfo{
			ID:        item.ID,
			CreatedAt: item.CreatedAt,
			PathCount: item.PathCount,
		})
	}
	return result, nil
}

func (c tuiCheckpointController) Plan(ctx context.Context, id string) (app.CheckpointPlan, error) {
	store, err := c.open()
	if err != nil {
		return app.CheckpointPlan{}, err
	}
	cp, err := store.LoadForWorkspace(id, c.workspace)
	if err != nil {
		return app.CheckpointPlan{}, err
	}
	plan, err := store.AnalyzeRestore(ctx, cp)
	if err != nil {
		return app.CheckpointPlan{}, err
	}
	result := app.CheckpointPlan{
		ID:    plan.CheckpointID,
		Noops: append([]string(nil), plan.Noops...),
	}
	for _, conflict := range plan.Conflicts {
		result.Conflicts = append(result.Conflicts, app.CheckpointConflict{
			Path:   conflict.Path,
			Action: string(conflict.Action),
		})
	}
	return result, nil
}

func (c tuiCheckpointController) Restore(ctx context.Context, id string) (app.CheckpointReport, error) {
	store, err := c.open()
	if err != nil {
		return app.CheckpointReport{}, err
	}
	cp, err := store.LoadForWorkspace(id, c.workspace)
	if err != nil {
		return app.CheckpointReport{}, err
	}
	report, err := store.Restore(ctx, cp, ionworkspace.RestoreOptions{AllowConflicts: true})
	if err != nil {
		return app.CheckpointReport{}, err
	}
	return app.CheckpointReport{
		Restored: append([]string(nil), report.Restored...),
		Removed:  append([]string(nil), report.Removed...),
	}, nil
}
