package tool

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"slices"
	"strings"
	"sync"
	"syscall"
	"time"
)

type localCommand struct {
	CWD               string
	Command           string
	Emit              func(localOutputUpdate) error
	Started           func(pid int) error
	PersistFullOutput bool
}

type localOutputUpdate struct {
	Text     string
	Snapshot bool
}

const (
	exitStdioGrace    = 100 * time.Millisecond
	exitStdioMaxDrain = 2 * time.Second
	processHandshake  = "IFS= read -r _ || exit 125\nexec \"$@\""
)

type localExecutor struct {
	sandbox     SandboxMode
	environment EnvironmentPolicy
	opts        commandOperations
}

type EnvironmentPolicy struct {
	mode  string
	allow map[string]struct{}
	deny  map[string]struct{}
}

const (
	executorEnvironmentAllowlist      = "allowlist"
	executorEnvironmentInherit        = "inherit"
	executorEnvironmentStripProviders = "inherit_without_provider_keys"
)

func NewEnvironmentPolicy(mode string, deny []string) EnvironmentPolicy {
	policy := NewAllowlistedEnvironmentPolicy(defaultEnvironmentAllowlist())
	switch strings.ToLower(strings.TrimSpace(mode)) {
	case executorEnvironmentInherit:
		policy.mode = executorEnvironmentInherit
	case executorEnvironmentStripProviders:
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

// NewAllowlistedEnvironmentPolicy creates a policy that passes only the
// explicitly named variables to a child process. It is the default runtime
// posture; credentials are not available unless the caller names them.
func NewAllowlistedEnvironmentPolicy(allow []string) EnvironmentPolicy {
	allowed := make(map[string]struct{}, len(allow))
	for _, key := range allow {
		key = strings.TrimSpace(key)
		if key != "" {
			allowed[key] = struct{}{}
		}
	}
	return EnvironmentPolicy{mode: executorEnvironmentAllowlist, allow: allowed}
}

func defaultEnvironmentAllowlist() []string {
	return []string{
		"COLORTERM", "GOCACHE", "GOMODCACHE", "GOPATH", "GOROOT", "HOME",
		"LANG", "LOGNAME", "PATH", "PWD", "SHELL", "TERM", "TERM_PROGRAM",
		"TMPDIR", "USER",
	}
}

func (p EnvironmentPolicy) Summary() string {
	switch p.mode {
	case executorEnvironmentInherit:
		return executorEnvironmentInherit
	case executorEnvironmentStripProviders:
		return executorEnvironmentStripProviders
	default:
		return executorEnvironmentAllowlist
	}
}

// AllowedVariables returns the policy identity recorded for approval. A star
// denotes an explicit inheritance escape hatch and is never the default.
func (p EnvironmentPolicy) AllowedVariables() []string {
	if p.mode == executorEnvironmentInherit {
		return []string{"*"}
	}
	if p.mode == executorEnvironmentStripProviders {
		return []string{"*", "!provider-credentials"}
	}
	allowed := make([]string, 0, len(p.allow))
	for key := range p.allow {
		allowed = append(allowed, key)
	}
	slices.Sort(allowed)
	return allowed
}

// CommandEnvironment returns the concrete environment for a child process.
// A nil result intentionally means inherit; callers should use the default
// allowlist policy unless they are implementing an explicit escape hatch.
func (p EnvironmentPolicy) CommandEnvironment() []string {
	return p.commandEnv()
}

func newLocalExecutorWithEnvironment(
	sandbox SandboxMode,
	environment EnvironmentPolicy,
) *localExecutor {
	return &localExecutor{
		sandbox:     sandbox,
		environment: environment,
		opts:        LocalOperations{},
	}
}

func (p EnvironmentPolicy) commandEnv() []string {
	switch p.mode {
	case executorEnvironmentInherit:
		return nil
	case executorEnvironmentStripProviders:
		return FilterEnvironment(os.Environ(), p.deny)
	default:
		return FilterEnvironmentAllowlist(os.Environ(), p.allow)
	}
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

// FilterEnvironmentAllowlist retains only variables named in allow. Invalid
// environment entries are dropped instead of being forwarded ambiguously.
func FilterEnvironmentAllowlist(env []string, allow map[string]struct{}) []string {
	out := make([]string, 0, len(env))
	for _, item := range env {
		key, _, ok := strings.Cut(item, "=")
		if !ok {
			continue
		}
		if _, permitted := allow[key]; permitted {
			out = append(out, item)
		}
	}
	return out
}

func (e *localExecutor) Run(ctx context.Context, request localCommand) (result string, runErr error) {
	plan, err := planSandboxedBash(request.CWD, request.Command, e.sandbox)
	if err != nil {
		return "", err
	}
	if plan.cleanup != nil {
		defer func() {
			if cleanupErr := plan.cleanup(); cleanupErr != nil {
				runErr = errors.Join(runErr, fmt.Errorf("sandbox cleanup: %w", cleanupErr))
			}
		}()
	}

	cmdArgs := append([]string{"-c", processHandshake, "ion-action", plan.name}, plan.args...)
	cmd := e.opts.CommandContext(ctx, "/bin/sh", cmdArgs...)
	cmd.Dir = plan.dir
	cmd.Env = e.environment.commandEnv()
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	handshake, err := cmd.StdinPipe()
	if err != nil {
		return "", fmt.Errorf("process handshake pipe: %w", err)
	}

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
		_ = handshake.Close()
		_ = stdoutWriter.Close()
		_ = stderrWriter.Close()
		return "", err
	}
	stopKill := context.AfterFunc(ctx, func() {
		if cmd.Process != nil {
			_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		}
	})
	defer stopKill()
	if request.Started != nil {
		if err := request.Started(cmd.Process.Pid); err != nil {
			_ = handshake.Close()
			_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
			_ = cmd.Wait()
			_ = stdoutWriter.Close()
			_ = stderrWriter.Close()
			return "", fmt.Errorf("record process group: %w", err)
		}
	}
	if _, err := io.WriteString(handshake, "ion-start\n"); err != nil {
		_ = handshake.Close()
		_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		_ = cmd.Wait()
		_ = stdoutWriter.Close()
		_ = stderrWriter.Close()
		return "", fmt.Errorf("release process handshake: %w", err)
	}
	if err := handshake.Close(); err != nil {
		_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		_ = cmd.Wait()
		_ = stdoutWriter.Close()
		_ = stderrWriter.Close()
		return "", fmt.Errorf("close process handshake: %w", err)
	}
	_ = stdoutWriter.Close()
	_ = stderrWriter.Close()

	output := newBashOutputAccumulator(request.PersistFullOutput)
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
						request.PersistFullOutput,
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
	result, resultErr := finalizeBashOutput(output, request.Emit, request.PersistFullOutput)
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
	persistFullOutput bool,
) (localOutputUpdate, bool, error) {
	if !output.truncated() {
		return localOutputUpdate{Text: string(data)}, true, nil
	}
	if *truncatedSnapshotEmitted {
		return localOutputUpdate{}, false, nil
	}
	snapshot, err := output.snapshot(persistFullOutput)
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
	persistFullOutput bool,
) (string, error) {
	snapshot, err := output.snapshot(persistFullOutput)
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
