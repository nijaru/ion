package tool

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"syscall"
	"time"
)

type localCommand struct {
	CWD     string
	Command string
	Emit    func(localOutputUpdate) error
}

type localOutputUpdate struct {
	Text     string
	Snapshot bool
}

const (
	exitStdioGrace    = 100 * time.Millisecond
	exitStdioMaxDrain = 2 * time.Second
)

type localExecutor struct {
	sandbox     SandboxMode
	environment EnvironmentPolicy
}

type EnvironmentPolicy struct {
	mode string
	deny map[string]struct{}
}

const (
	executorEnvironmentInherit        = "inherit"
	executorEnvironmentStripProviders = "inherit_without_provider_keys"
)

func NewEnvironmentPolicy(mode string, deny []string) EnvironmentPolicy {
	policy := EnvironmentPolicy{mode: executorEnvironmentInherit}
	if mode == executorEnvironmentStripProviders {
		policy.mode = executorEnvironmentStripProviders
		policy.deny = make(map[string]struct{}, len(deny))
		for _, key := range deny {
			key = strings.TrimSpace(key)
			if key != "" {
				policy.deny[key] = struct{}{}
			}
		}
	}
	return policy
}

func (p EnvironmentPolicy) Summary() string {
	if p.mode == executorEnvironmentStripProviders {
		return executorEnvironmentStripProviders
	}
	return executorEnvironmentInherit
}

func newLocalExecutor(sandbox SandboxMode) *localExecutor {
	return newLocalExecutorWithEnvironment(
		sandbox,
		NewEnvironmentPolicy(executorEnvironmentInherit, nil),
	)
}

func newLocalExecutorWithEnvironment(
	sandbox SandboxMode,
	environment EnvironmentPolicy,
) *localExecutor {
	return &localExecutor{sandbox: sandbox, environment: environment}
}

func (p EnvironmentPolicy) commandEnv() []string {
	if p.mode != executorEnvironmentStripProviders {
		return nil
	}
	return FilterEnvironment(os.Environ(), p.deny)
}

func FilterEnvironment(env []string, deny map[string]struct{}) []string {
	if len(deny) == 0 {
		return env
	}
	out := make([]string, 0, len(env))
	for _, item := range env {
		key, _, ok := strings.Cut(item, "=")
		if !ok {
			out = append(out, item)
			continue
		}
		if _, blocked := deny[key]; blocked {
			continue
		}
		out = append(out, item)
	}
	return out
}

func (e *localExecutor) Run(ctx context.Context, request localCommand) (string, error) {
	plan, err := planSandboxedBash(request.CWD, request.Command, e.sandbox)
	if err != nil {
		return "", err
	}
	if plan.cleanup != nil {
		defer func() { _ = plan.cleanup() }()
	}

	cmd := exec.CommandContext(ctx, plan.name, plan.args...)
	cmd.Dir = plan.dir
	cmd.Env = e.environment.commandEnv()
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}

	stdout, stdoutWriter, err := pipeForCommand()
	if err != nil {
		return "", fmt.Errorf("stdout pipe: %w", err)
	}
	defer stdout.Close()
	stderr, stderrWriter, err := pipeForCommand()
	if err != nil {
		_ = stdoutWriter.Close()
		return "", fmt.Errorf("stderr pipe: %w", err)
	}
	defer stderr.Close()
	cmd.Stdout = stdoutWriter
	cmd.Stderr = stderrWriter

	if err := cmd.Start(); err != nil {
		_ = stdoutWriter.Close()
		_ = stderrWriter.Close()
		return "", err
	}
	_ = stdoutWriter.Close()
	_ = stderrWriter.Close()

	stopKill := context.AfterFunc(ctx, func() {
		if cmd.Process != nil {
			_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		}
	})
	defer stopKill()

	output := newBashOutputAccumulator()
	var mu sync.Mutex
	var wg sync.WaitGroup
	readProgress := make(chan struct{}, 1)
	noteReadProgress := func() {
		select {
		case readProgress <- struct{}{}:
		default:
		}
	}

	truncatedSnapshotEmitted := false
	var emitErr error
	hasEmitErr := func() bool {
		mu.Lock()
		defer mu.Unlock()
		return emitErr != nil
	}
	readPipe := func(r io.Reader) {
		buf := make([]byte, 4096)
		for {
			n, err := r.Read(buf)
			if n > 0 {
				noteReadProgress()
				if hasEmitErr() {
					return
				}
				data := bytesClone(buf[:n])
				mu.Lock()
				if err := output.append(data); err != nil {
					if emitErr == nil {
						emitErr = err
						if cmd.Process != nil {
							_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
						}
					}
					mu.Unlock()
					return
				}
				if request.Emit != nil {
					update, ok, err := bashOutputUpdateForChunk(
						output,
						data,
						&truncatedSnapshotEmitted,
					)
					if err != nil {
						if emitErr == nil {
							emitErr = err
							if cmd.Process != nil {
								_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
							}
						}
						mu.Unlock()
						return
					}
					if ok {
						if err := request.Emit(update); err != nil {
							if emitErr == nil {
								emitErr = err
								if cmd.Process != nil {
									_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
								}
							}
							mu.Unlock()
							return
						}
					}
				}
				mu.Unlock()
			}
			if err != nil {
				break
			}
		}
	}

	wg.Go(func() { readPipe(stdout) })
	wg.Go(func() { readPipe(stderr) })

	err = cmd.Wait()
	waitForReadersOrClosePipes(&wg, cmd.Process.Pid, readProgress, stdout, stderr)

	mu.Lock()
	result, resultErr := finalizeBashOutput(output, request.Emit)
	if emitErr == nil && resultErr != nil {
		emitErr = resultErr
	}
	mu.Unlock()
	if emitErr != nil {
		return result, emitErr
	}
	if err != nil {
		if result == "" {
			return "", err
		}
		return result, err
	}

	return result, nil
}

func bashOutputUpdateForChunk(
	output *bashOutputAccumulator,
	data []byte,
	truncatedSnapshotEmitted *bool,
) (localOutputUpdate, bool, error) {
	if !output.truncated() {
		return localOutputUpdate{Text: string(data)}, true, nil
	}
	if *truncatedSnapshotEmitted {
		return localOutputUpdate{}, false, nil
	}
	snapshot, err := output.snapshot(true)
	if err != nil {
		return localOutputUpdate{}, false, err
	}
	*truncatedSnapshotEmitted = true
	return localOutputUpdate{
		Text:     formatBashSnapshot(snapshot, output, ""),
		Snapshot: true,
	}, true, nil
}

func finalizeBashOutput(
	output *bashOutputAccumulator,
	emit func(localOutputUpdate) error,
) (string, error) {
	snapshot, err := output.snapshot(true)
	if err != nil {
		return "", err
	}
	result := formatBashSnapshot(snapshot, output, "")
	if err := output.closeTempFile(); err != nil {
		return result, err
	}
	if emit != nil && snapshot.Truncation.Truncated {
		if err := emit(localOutputUpdate{
			Text:     result,
			Snapshot: true,
		}); err != nil {
			return result, err
		}
	}
	return result, nil
}

func bytesClone(data []byte) []byte {
	out := make([]byte, len(data))
	copy(out, data)
	return out
}

func pipeForCommand() (*os.File, *os.File, error) {
	reader, writer, err := os.Pipe()
	if err != nil {
		return nil, nil, err
	}
	return reader, writer, nil
}

func waitForReadersOrClosePipes(
	wg *sync.WaitGroup,
	processID int,
	progress <-chan struct{},
	readers ...*os.File,
) {
	done := make(chan struct{})
	go func() {
		wg.Wait()
		close(done)
	}()

	idleTimer := time.NewTimer(exitStdioGrace)
	defer idleTimer.Stop()
	maxTimer := time.NewTimer(exitStdioMaxDrain)
	defer maxTimer.Stop()

	resetIdleTimer := func() {
		if !idleTimer.Stop() {
			select {
			case <-idleTimer.C:
			default:
			}
		}
		idleTimer.Reset(exitStdioGrace)
	}

	for {
		select {
		case <-done:
			return
		case <-progress:
			resetIdleTimer()
		case <-idleTimer.C:
			closeReadersAndWait(processID, readers, done)
			return
		case <-maxTimer.C:
			closeReadersAndWait(processID, readers, done)
			return
		}
	}
}

func closeReadersAndWait(processID int, readers []*os.File, done <-chan struct{}) {
	if processID > 0 {
		_ = syscall.Kill(-processID, syscall.SIGKILL)
	}
	for _, reader := range readers {
		_ = reader.Close()
	}
	<-done
}
