package tools

import (
	"context"
	"fmt"
	"io"
	"os/exec"
	"slices"
	"strings"
	"sync"
	"syscall"
)

type BackgroundJobInfo struct {
	ID          string
	Command     string
	Status      string
	OutputBytes int
}

type backgroundJobs struct {
	mu     sync.Mutex
	next   int
	active map[string]*backgroundJob
}

func newBackgroundJobs() *backgroundJobs {
	return &backgroundJobs{active: make(map[string]*backgroundJob)}
}

func (j *backgroundJobs) start(
	ctx context.Context,
	executor *localExecutor,
	request localCommand,
) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	plan, err := planSandboxedBash(request.CWD, request.Command, executor.sandbox)
	if err != nil {
		return "", err
	}

	cmd := exec.CommandContext(context.Background(), plan.name, plan.args...)
	cmd.Dir = plan.dir
	cmd.Env = executor.environment.commandEnv()
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		if plan.cleanup != nil {
			_ = plan.cleanup()
		}
		return "", fmt.Errorf("stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		if plan.cleanup != nil {
			_ = plan.cleanup()
		}
		return "", fmt.Errorf("stderr pipe: %w", err)
	}

	j.mu.Lock()
	j.next++
	id := fmt.Sprintf("bash-%d", j.next)
	job := &backgroundJob{
		id:      id,
		command: request.Command,
		cmd:     cmd,
		done:    make(chan struct{}),
		cleanup: plan.cleanup,
	}
	j.active[id] = job
	j.mu.Unlock()

	if err := cmd.Start(); err != nil {
		j.remove(id)
		if plan.cleanup != nil {
			_ = plan.cleanup()
		}
		return "", err
	}

	go job.run(stdout, stderr)
	return fmt.Sprintf("background job %s started", id), nil
}

func (j *backgroundJobs) list() []BackgroundJobInfo {
	j.mu.Lock()
	jobs := make([]*backgroundJob, 0, len(j.active))
	for _, job := range j.active {
		jobs = append(jobs, job)
	}
	j.mu.Unlock()

	infos := make([]BackgroundJobInfo, 0, len(jobs))
	for _, job := range jobs {
		infos = append(infos, job.info())
	}
	slices.SortFunc(infos, func(a, b BackgroundJobInfo) int {
		return strings.Compare(a.ID, b.ID)
	})
	return infos
}

func (j *backgroundJobs) output(id string, tailLines int) (string, error) {
	job, ok := j.lookup(id)
	if !ok {
		return "", fmt.Errorf("unknown background job %q", id)
	}
	return job.output(tailLines), nil
}

func (j *backgroundJobs) kill(ctx context.Context, id string) (string, error) {
	job, ok := j.lookup(id)
	if !ok {
		return "", fmt.Errorf("unknown background job %q", id)
	}
	if err := job.stop(ctx); err != nil {
		return "", err
	}
	return fmt.Sprintf("background job %s stopped", id), nil
}

func (j *backgroundJobs) close() {
	j.mu.Lock()
	jobs := make([]*backgroundJob, 0, len(j.active))
	for _, job := range j.active {
		jobs = append(jobs, job)
	}
	j.mu.Unlock()

	for _, job := range jobs {
		_ = job.stop(context.Background())
	}
}

func (j *backgroundJobs) lookup(id string) (*backgroundJob, bool) {
	id = strings.TrimSpace(id)
	if id == "" {
		return nil, false
	}
	j.mu.Lock()
	defer j.mu.Unlock()
	job, ok := j.active[id]
	return job, ok
}

func (j *backgroundJobs) remove(id string) {
	j.mu.Lock()
	delete(j.active, id)
	j.mu.Unlock()
}

type backgroundJob struct {
	id      string
	command string
	cmd     *exec.Cmd
	done    chan struct{}
	cleanup func() error

	mu       sync.Mutex
	buffer   strings.Builder
	truncate bool
	stopped  bool
	err      error
}

func (j *backgroundJob) run(stdout, stderr io.Reader) {
	var wg sync.WaitGroup
	wg.Go(func() { j.read(stdout) })
	wg.Go(func() { j.read(stderr) })
	err := j.cmd.Wait()
	wg.Wait()
	if j.cleanup != nil {
		_ = j.cleanup()
	}
	j.mu.Lock()
	j.err = err
	j.mu.Unlock()
	close(j.done)
}

func (j *backgroundJob) read(r io.Reader) {
	buf := make([]byte, 4096)
	for {
		n, err := r.Read(buf)
		if n > 0 {
			j.append(buf[:n])
		}
		if err != nil {
			return
		}
	}
}

func (j *backgroundJob) append(chunk []byte) {
	j.mu.Lock()
	defer j.mu.Unlock()
	if j.buffer.Len() >= maxOutputSize {
		if !j.truncate {
			j.truncate = true
			j.buffer.WriteString("\n... [Output truncated: exceeded 1MB limit] ...\n")
		}
		return
	}
	j.buffer.Write(chunk)
}

func (j *backgroundJob) stop(ctx context.Context) error {
	j.mu.Lock()
	if !j.stopped {
		j.stopped = true
		if j.cmd.Process != nil {
			_ = syscall.Kill(-j.cmd.Process.Pid, syscall.SIGKILL)
		}
	}
	j.mu.Unlock()

	select {
	case <-j.done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (j *backgroundJob) info() BackgroundJobInfo {
	j.mu.Lock()
	defer j.mu.Unlock()
	return BackgroundJobInfo{
		ID:          j.id,
		Command:     j.command,
		Status:      j.statusLocked(),
		OutputBytes: j.buffer.Len(),
	}
}

func (j *backgroundJob) output(tailLines int) string {
	j.mu.Lock()
	status := j.statusLocked()
	output := j.buffer.String()
	j.mu.Unlock()

	output = tail(output, tailLines)
	if strings.TrimSpace(output) == "" {
		return fmt.Sprintf("background job %s %s with no output", j.id, status)
	}
	return fmt.Sprintf("background job %s %s\n%s", j.id, status, output)
}

func (j *backgroundJob) statusLocked() string {
	select {
	case <-j.done:
		if j.stopped {
			return "stopped"
		}
		if j.err != nil {
			return "failed"
		}
		return "done"
	default:
		return "running"
	}
}

func tail(output string, lines int) string {
	if lines <= 0 {
		return output
	}
	parts := strings.SplitAfter(output, "\n")
	if len(parts) > 0 && parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	if len(parts) <= lines {
		return output
	}
	return strings.Join(parts[len(parts)-lines:], "")
}
