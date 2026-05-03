package tools

import (
	"context"
	"fmt"
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

const maxOutputSize = 1024 * 1024 // 1MB

type Bash struct {
	cwd      string
	executor *localExecutor
}

func NewBash(cwd string) *Bash {
	return &Bash{
		cwd:      cwd,
		executor: newLocalExecutor(resolveSandboxMode()),
	}
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

func (b *Bash) ExecuteStreaming(
	ctx context.Context,
	args string,
	emit func(string) error,
) (string, error) {
	var input struct {
		Command string `json:"command"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
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
