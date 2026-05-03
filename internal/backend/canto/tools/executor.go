package tools

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"syscall"
)

type localCommand struct {
	CWD     string
	Command string
	Emit    func(string) error
}

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
	return filterEnvironment(os.Environ(), p.deny)
}

func filterEnvironment(env []string, deny map[string]struct{}) []string {
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

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return "", fmt.Errorf("stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return "", fmt.Errorf("stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return "", err
	}

	stopKill := context.AfterFunc(ctx, func() {
		if cmd.Process != nil {
			_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		}
	})
	defer stopKill()

	var output strings.Builder
	var mu sync.Mutex
	var wg sync.WaitGroup
	wg.Add(2)

	limitExceeded := false
	readPipe := func(r io.Reader) {
		defer wg.Done()
		buf := make([]byte, 4096)
		for {
			n, err := r.Read(buf)
			if n > 0 {
				mu.Lock()
				if output.Len() >= maxOutputSize {
					if !limitExceeded {
						limitExceeded = true
						output.WriteString("\n... [Output truncated: exceeded 1MB limit] ...\n")
					}
					mu.Unlock()
					continue
				}
				chunk := string(buf[:n])
				output.WriteString(chunk)
				if request.Emit != nil {
					_ = request.Emit(chunk)
				}
				mu.Unlock()
			}
			if err != nil {
				break
			}
		}
	}

	go readPipe(stdout)
	go readPipe(stderr)

	err = cmd.Wait()
	wg.Wait()
	result := limitToolOutput(output.String())
	if err != nil {
		if result == "" {
			return "", err
		}
		return result, err
	}

	return result, nil
}
