package app

import (
	"context"
	"time"
)

// CheckpointInfo is the TUI projection of one file-mutation checkpoint.
// Checkpoints are workspace recovery metadata, not session entries.
type CheckpointInfo struct {
	ID        string
	CreatedAt time.Time
	PathCount int
}

type CheckpointConflict struct {
	Path   string
	Action string
}

type CheckpointPlan struct {
	ID        string
	Conflicts []CheckpointConflict
	Noops     []string
}

type CheckpointReport struct {
	Restored []string
	Removed  []string
}

// CheckpointController keeps filesystem recovery behind the command host. The
// app package never owns checkpoint storage or file restoration policy.
type CheckpointController interface {
	List(ctx context.Context, limit int) ([]CheckpointInfo, error)
	Plan(ctx context.Context, id string) (CheckpointPlan, error)
	Restore(ctx context.Context, id string) (CheckpointReport, error)
}

type checkpointListMsg struct {
	generation uint64
	requestID  uint64
	items      []CheckpointInfo
	err        error
}

type checkpointPlanMsg struct {
	generation uint64
	requestID  uint64
	plan       CheckpointPlan
	err        error
}

type checkpointRestoredMsg struct {
	generation uint64
	requestID  uint64
	id         string
	report     CheckpointReport
	err        error
}
