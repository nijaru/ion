package tool

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

// JobStatus describes the lifecycle of a runtime-owned background command.
type JobStatus string

const (
	JobStarting  JobStatus = "starting"
	JobRunning   JobStatus = "running"
	JobCompleted JobStatus = "completed"
	JobFailed    JobStatus = "failed"
	JobCanceled  JobStatus = "canceled"
)

// JobSnapshot is the read-only projection of a managed background command.
// Jobs are intentionally runtime-ephemeral; they are not session entries.
type JobSnapshot struct {
	ID         string
	Command    string
	Status     JobStatus
	Output     string
	Error      string
	StartedAt  time.Time
	FinishedAt time.Time
}

type jobRecord struct {
	info   JobSnapshot
	output jobOutput
	cancel context.CancelFunc
	done   chan struct{}
}

type jobRunner func(
	context.Context,
	func(pid int),
	func(localOutputUpdate) error,
) (string, error)

// JobManager owns background command state and the cancellation context that
// outlives an individual model turn but ends with the Ion process.
type JobManager struct {
	mu     sync.Mutex
	root   context.Context
	cancel context.CancelFunc
	jobs   map[string]*jobRecord
	nextID uint64
	closed bool
}

const maxRetainedJobs = 128

func NewJobManager() *JobManager {
	root, cancel := context.WithCancel(context.Background())
	return &JobManager{
		root:   root,
		cancel: cancel,
		jobs:   make(map[string]*jobRecord),
	}
}

// start launches a job and does not inherit the caller's cancellation after
// the process-start acknowledgement. If the caller is canceled before that
// acknowledgement, the job is canceled and reaped before start returns.
func (m *JobManager) start(ctx context.Context, command string, run jobRunner) (string, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	if run == nil {
		return "", errors.New("job runner is nil")
	}
	select {
	case <-ctx.Done():
		return "", ctx.Err()
	default:
	}

	m.mu.Lock()
	if m.closed {
		m.mu.Unlock()
		return "", errors.New("job manager is closed")
	}
	m.nextID++
	id := fmt.Sprintf("job-%d", m.nextID)
	jobCtx, cancel := context.WithCancel(m.root)
	// Background jobs intentionally outlive the turn's cancellation, but
	// action-boundary capabilities must follow the process that they authorize.
	// Propagate those explicit values onto the runtime-owned job context while
	// retaining JobManager shutdown as the cancellation parent.
	if recorder, ok := ProcessGroupRecorderFromContext(ctx); ok {
		jobCtx = WithProcessGroupRecorder(jobCtx, recorder)
	}
	if guard, ok := ActionPathGuardFromContext(ctx); ok {
		jobCtx = WithActionPathGuard(jobCtx, guard.Paths)
	}
	var lifecycle JobLifecycleRecorder
	if value, ok := JobLifecycleRecorderFromContext(ctx); ok {
		lifecycle = value
		jobCtx = WithJobLifecycleRecorder(jobCtx, lifecycle)
	}
	record := &jobRecord{
		info: JobSnapshot{
			ID:        id,
			Command:   command,
			Status:    JobStarting,
			StartedAt: time.Now(),
		},
		cancel: cancel,
		done:   make(chan struct{}),
	}
	m.jobs[id] = record
	m.pruneLocked()
	m.mu.Unlock()

	ready := make(chan error, 1)
	var readyOnce sync.Once
	signalReady := func(_ int) {
		m.mu.Lock()
		if current := m.jobs[id]; current != nil && current.info.Status == JobStarting {
			current.info.Status = JobRunning
		}
		m.mu.Unlock()
		readyOnce.Do(func() { ready <- nil })
	}

	go func() {
		result, err := run(jobCtx, signalReady, func(update localOutputUpdate) error {
			return m.appendOutput(id, update)
		})
		readyOnce.Do(func() { ready <- err })
		m.finish(id, result, err)
		if lifecycle.Finished != nil {
			lifecycle.Finished(result, err)
		}
	}()

	select {
	case err := <-ready:
		if err == nil {
			if lifecycle.Started != nil {
				if err := lifecycle.Started(id); err != nil {
					record.cancel()
					<-record.done
					return "", fmt.Errorf("register job %s: %w", id, err)
				}
			}
			return id, nil
		}
		<-record.done
		return "", fmt.Errorf("start job %s: %w", id, err)
	case <-ctx.Done():
		record.cancel()
		<-record.done
		return "", ctx.Err()
	case <-m.root.Done():
		record.cancel()
		<-record.done
		return "", errors.New("job manager is closed")
	}
}

func (m *JobManager) appendOutput(id string, update localOutputUpdate) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	record := m.jobs[id]
	if record == nil {
		return fmt.Errorf("job %q not found", id)
	}
	record.output.Append(update)
	return nil
}

func (m *JobManager) finish(id, result string, err error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	record := m.jobs[id]
	if record == nil {
		return
	}
	if record.output.Empty() && result != "" {
		record.output.Append(localOutputUpdate{Text: result})
	}
	record.info.Output = record.output.String()
	record.info.FinishedAt = time.Now()
	switch {
	case err == nil:
		if record.info.Status == JobStarting {
			record.info.Status = JobRunning
		}
		record.info.Status = JobCompleted
	case errors.Is(err, context.Canceled):
		record.info.Status = JobCanceled
		record.info.Error = "canceled"
	default:
		record.info.Status = JobFailed
		record.info.Error = err.Error()
	}
	close(record.done)
}

// List returns all jobs in launch order, including completed jobs retained for
// this Ion process. Output is bounded by the job output tail policy.
func (m *JobManager) List() []JobSnapshot {
	m.mu.Lock()
	defer m.mu.Unlock()
	ids := make([]string, 0, len(m.jobs))
	for id := range m.jobs {
		ids = append(ids, id)
	}
	slices.SortFunc(ids, func(a, b string) int {
		return compareJobIDs(a, b)
	})
	out := make([]JobSnapshot, 0, len(ids))
	for _, id := range ids {
		out = append(out, m.snapshotLocked(m.jobs[id]))
	}
	return out
}

// Get returns one job snapshot.
func (m *JobManager) Get(id string) (JobSnapshot, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	record := m.jobs[strings.TrimSpace(id)]
	if record == nil {
		return JobSnapshot{}, fmt.Errorf("job %q not found", id)
	}
	return m.snapshotLocked(record), nil
}

// Stop cancels a running job and waits until its process group has been
// reaped. Stopping a completed job is an explicit error so stale IDs are
// visible to both the model and the user.
func (m *JobManager) Stop(id string) error {
	id = strings.TrimSpace(id)
	m.mu.Lock()
	record := m.jobs[id]
	if record == nil {
		m.mu.Unlock()
		return fmt.Errorf("job %q not found", id)
	}
	if record.info.Status != JobStarting && record.info.Status != JobRunning {
		status := record.info.Status
		m.mu.Unlock()
		return fmt.Errorf("job %q is not running (status %s)", id, status)
	}
	cancel := record.cancel
	done := record.done
	m.mu.Unlock()
	cancel()
	<-done
	return nil
}

// Close cancels every live job and waits for all process groups to be reaped.
// It is safe to call more than once.
func (m *JobManager) Close() error {
	m.mu.Lock()
	if m.closed {
		m.mu.Unlock()
		return nil
	}
	m.closed = true
	m.cancel()
	records := make([]*jobRecord, 0, len(m.jobs))
	for _, record := range m.jobs {
		records = append(records, record)
	}
	m.mu.Unlock()

	for _, record := range records {
		<-record.done
	}
	return nil
}

func (m *JobManager) snapshotLocked(record *jobRecord) JobSnapshot {
	info := record.info
	info.Output = record.output.String()
	return info
}

func (m *JobManager) pruneLocked() {
	if len(m.jobs) <= maxRetainedJobs {
		return
	}
	ids := make([]string, 0, len(m.jobs))
	for id := range m.jobs {
		ids = append(ids, id)
	}
	slices.SortFunc(ids, compareJobIDs)
	for _, id := range ids {
		if len(m.jobs) <= maxRetainedJobs {
			return
		}
		record := m.jobs[id]
		if record.info.Status == JobStarting || record.info.Status == JobRunning {
			continue
		}
		delete(m.jobs, id)
	}
}

func compareJobIDs(a, b string) int {
	// IDs are generated as job-<monotonic number>; parsing keeps job-10 after
	// job-9 while remaining deterministic if a caller supplies a malformed key.
	var an, bn uint64
	if _, err := fmt.Sscanf(a, "job-%d", &an); err != nil {
		return strings.Compare(a, b)
	}
	if _, err := fmt.Sscanf(b, "job-%d", &bn); err != nil {
		return strings.Compare(a, b)
	}
	if an < bn {
		return -1
	}
	if an > bn {
		return 1
	}
	return 0
}

type jobOutput struct {
	data      []byte
	truncated bool
}

func (o *jobOutput) Append(update localOutputUpdate) {
	if update.Snapshot {
		o.data = nil
		o.truncated = true
	}
	if update.Text == "" {
		return
	}
	text := strings.ReplaceAll(update.Text, "Full output: (unavailable)", "full output omitted for runtime job")
	o.data = append(o.data, text...)
	for bytes.Count(o.data, []byte{'\n'}) > bashMaxOutputLines {
		idx := bytes.IndexByte(o.data, '\n')
		if idx < 0 {
			break
		}
		o.data = o.data[idx+1:]
		o.truncated = true
	}
	if len(o.data) > MaxToolOutputSize {
		start := len(o.data) - MaxToolOutputSize
		for start < len(o.data) && !utf8.RuneStart(o.data[start]) {
			start++
		}
		if start >= len(o.data) {
			o.data = nil
		} else {
			o.data = bytes.Clone(o.data[start:])
		}
		o.truncated = true
	}
}

func (o *jobOutput) Empty() bool { return len(o.data) == 0 }

func (o *jobOutput) String() string {
	if len(o.data) == 0 {
		return ""
	}
	text := string(o.data)
	if o.truncated {
		return "[job output truncated; showing tail]\n" + text
	}
	return text
}
