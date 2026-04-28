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

const maxVerifyOutputSize = 1024 * 1024 // 1MB

// Verify tool runs a command and provides structured feedback for auto-verification.
type Verify struct {
	CWD      string
	Callback func(command string, passed bool, metric string, output string)
}

func (v *Verify) Spec() llm.Spec {
	return llm.Spec{
		Name:        "verify",
		Description: "Execute a verification command (test, lint, build) and report structured results.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"command": map[string]any{
					"type":        "string",
					"description": "The command to run (e.g. 'go test ./...', 'npm test').",
				},
			},
			"required": []string{"command"},
		},
	}
}

func (v *Verify) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Command string `json:"command"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	cmd := exec.CommandContext(ctx, "bash", "-c", input.Command)
	cmd.Dir = v.CWD
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
	handlePipe := func(r io.Reader) {
		defer wg.Done()
		buf := make([]byte, 4096)
		for {
			n, err := r.Read(buf)
			if n > 0 {
				mu.Lock()
				if output.Len() >= maxVerifyOutputSize {
					if !limitExceeded {
						limitExceeded = true
						output.WriteString("\n... [Output truncated: exceeded 1MB limit] ...\n")
					}
					mu.Unlock()
					continue
				}
				output.WriteString(string(buf[:n]))
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

	passed := err == nil
	outStr := output.String()
	metric := "Exit Code 0"
	if !passed {
		metric = fmt.Sprintf("Error: %v", err)
	}

	if v.Callback != nil {
		v.Callback(input.Command, passed, metric, outStr)
	}

	status := "PASSED"
	if !passed {
		status = "FAILED"
	}

	return fmt.Sprintf("Verification %s: %s\n\nOutput:\n%s", status, input.Command, outStr), nil
}
