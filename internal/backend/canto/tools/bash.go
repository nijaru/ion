package tools

import (
	"context"
	"fmt"
	"io"
	"os/exec"
	"strings"
	"sync"
	"syscall"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

const maxOutputSize = 1024 * 1024 // 1MB

type Bash struct {
	cwd    string
	sandbox SandboxMode
}

func NewBash(cwd string) *Bash {
	return &Bash{cwd: cwd, sandbox: resolveSandboxMode()}
}

func (b *Bash) Spec() llm.Spec {
	return llm.Spec{
		Name:        "bash",
		Description: "Run a shell command in the current working directory. Always prefer non-interactive commands (e.g. use --yes flags) to prevent hanging the TUI.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"command": map[string]any{
					"type":        "string",
					"description": "The command to execute (e.g. 'ls -la', 'go test ./...', 'git status')",
				},
			},
			"required": []string{"command"},
		},
	}
}

func (b *Bash) Execute(ctx context.Context, args string) (string, error) {
	return b.ExecuteStreaming(ctx, args, nil)
}

func (b *Bash) ExecuteStreaming(ctx context.Context, args string, emit func(string) error) (string, error) {
	var input struct {
		Command string `json:"command"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	plan, err := planSandboxedBash(b.cwd, input.Command, b.sandbox)
	if err != nil {
		return "", err
	}
	if plan.cleanup != nil {
		defer func() { _ = plan.cleanup() }()
	}

	cmd := exec.CommandContext(ctx, plan.name, plan.args...)
	cmd.Dir = plan.dir
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

	// Ensure process group is killed on exit to prevent orphan leaks
	defer func() {
		if cmd.Process != nil {
			_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		}
	}()

	var output strings.Builder
	var mu sync.Mutex
	var wg sync.WaitGroup
	wg.Add(2)
	
	limitExceeded := false

	// Helper to handle pipe output and emit deltas
	handlePipe := func(r io.Reader) {
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
				if emit != nil {
					_ = emit(chunk)
				}
				mu.Unlock()
			}
			if err != nil {
				break
			}
		}
	}

	go handlePipe(stdout)
	go handlePipe(stderr)

	err = cmd.Wait()
	wg.Wait()
	res := output.String()
	
	if err != nil {
		if res == "" {
			return "", err
		}
		return res + "\nError: " + err.Error(), nil
	}

	return res, nil
}
