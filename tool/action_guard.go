package tool

import (
	"context"
	"slices"
)

// ActionPathGuard carries the canonical paths approved by the runtime action
// planner. It is an execution capability, not model-visible tool input.
type ActionPathGuard struct {
	Paths []string
}

// ProcessGroupRecorder is called immediately after a process-backed effect
// starts. A runtime action boundary uses it to durably associate the process
// group with the already-started action for crash cleanup and recovery.
type ProcessGroupRecorder func(pid int) error

// JobLifecycleRecorder lets the runtime action boundary distinguish a
// background launch from a foreground result. Started is acknowledged before
// the job start call returns; Finished runs after the managed process has been
// reaped. A Finished error is retained on the job projection so asynchronous
// journal failures remain visible to the user.
type JobLifecycleRecorder struct {
	Started  func(jobID string) error
	Finished func(result string, err error) error
}

type actionPathGuardKey struct{}

type processGroupRecorderKey struct{}

type jobLifecycleRecorderKey struct{}

// WithActionPathGuard attaches the approved canonical targets to an effect
// callback context. Built-in mutating file tools enforce the guard immediately
// before their atomic replacement.
func WithActionPathGuard(ctx context.Context, paths []string) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	return context.WithValue(ctx, actionPathGuardKey{}, ActionPathGuard{Paths: slices.Clone(paths)})
}

// ActionPathGuardFromContext retrieves the runtime-approved canonical targets.
func ActionPathGuardFromContext(ctx context.Context) (ActionPathGuard, bool) {
	if ctx == nil {
		return ActionPathGuard{}, false
	}
	guard, ok := ctx.Value(actionPathGuardKey{}).(ActionPathGuard)
	if !ok {
		return ActionPathGuard{}, false
	}
	guard.Paths = slices.Clone(guard.Paths)
	return guard, true
}

// WithProcessGroupRecorder attaches the runtime-owned process identity hook
// to an effect callback context.
func WithProcessGroupRecorder(ctx context.Context, recorder ProcessGroupRecorder) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	return context.WithValue(ctx, processGroupRecorderKey{}, recorder)
}

// ProcessGroupRecorderFromContext retrieves the process identity hook.
func ProcessGroupRecorderFromContext(ctx context.Context) (ProcessGroupRecorder, bool) {
	if ctx == nil {
		return nil, false
	}
	recorder, ok := ctx.Value(processGroupRecorderKey{}).(ProcessGroupRecorder)
	return recorder, ok && recorder != nil
}

// WithJobLifecycleRecorder attaches runtime-owned background-job lifecycle
// callbacks to a tool execution context.
func WithJobLifecycleRecorder(ctx context.Context, recorder JobLifecycleRecorder) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	return context.WithValue(ctx, jobLifecycleRecorderKey{}, recorder)
}

// JobLifecycleRecorderFromContext retrieves background-job lifecycle callbacks.
func JobLifecycleRecorderFromContext(ctx context.Context) (JobLifecycleRecorder, bool) {
	if ctx == nil {
		return JobLifecycleRecorder{}, false
	}
	recorder, ok := ctx.Value(jobLifecycleRecorderKey{}).(JobLifecycleRecorder)
	return recorder, ok && (recorder.Started != nil || recorder.Finished != nil)
}
