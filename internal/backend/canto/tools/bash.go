package tools

import (
	"context"
	"fmt"
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

type Bash struct {
	cwd      string
	executor *localExecutor
}

func NewBash(cwd string) *Bash {
	return NewBashWithEnvironment(
		cwd,
		NewEnvironmentPolicy(executorEnvironmentInherit, nil),
	)
}

func NewBashWithEnvironment(
	cwd string,
	environment EnvironmentPolicy,
) *Bash {
	return &Bash{
		cwd:      cwd,
		executor: newLocalExecutorWithEnvironment(resolveSandboxMode(), environment),
	}
}

func (b *Bash) Spec() llm.Spec {
	properties := map[string]any{
		"command": map[string]any{
			"type":        "string",
			"description": "The command to execute (e.g. 'ls -la', 'go test ./...', 'git status')",
		},
	}

	return llm.Spec{
		Name:        "bash",
		Description: "Run a shell command in the current working directory. Always prefer non-interactive commands (e.g. use --yes flags) to prevent hanging the TUI.",
		Parameters: map[string]any{
			"type":       "object",
			"properties": properties,
		},
	}
}

func (b *Bash) Execute(ctx context.Context, args string) (string, error) {
	return b.ExecuteStreaming(ctx, args, nil)
}

func (b *Bash) ExecuteStreaming(
	ctx context.Context,
	args string,
	emit func(string) error,
) (string, error) {
	var input struct {
		Command    string `json:"command"`
		Action     string `json:"action"`
		Background bool   `json:"background"`
		JobID      string `json:"job_id"`
		TailLines  int    `json:"tail_lines"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	action := strings.ToLower(strings.TrimSpace(input.Action))
	if action != "" && action != "run" ||
		input.Background ||
		strings.TrimSpace(input.JobID) != "" ||
		input.TailLines != 0 {
		return "", fmt.Errorf("background jobs are deferred")
	}
	if strings.TrimSpace(input.Command) == "" {
		return "", fmt.Errorf("command is required")
	}
	return b.executor.Run(ctx, localCommand{
		CWD:     b.cwd,
		Command: input.Command,
		Emit:    emit,
	})
}
