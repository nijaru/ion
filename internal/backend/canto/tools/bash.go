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
	jobs     *backgroundJobs
}

type BashOption func(*bashOptions)

type bashOptions struct {
	backgroundJobs bool
}

func WithBackgroundJobs() BashOption {
	return func(opts *bashOptions) {
		opts.backgroundJobs = true
	}
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
	opts ...BashOption,
) *Bash {
	options := bashOptions{}
	for _, opt := range opts {
		opt(&options)
	}

	b := &Bash{
		cwd:      cwd,
		executor: newLocalExecutorWithEnvironment(resolveSandboxMode(), environment),
	}
	if options.backgroundJobs {
		b.jobs = newBackgroundJobs()
	}
	return b
}

func (b *Bash) Spec() llm.Spec {
	properties := map[string]any{
		"command": map[string]any{
			"type":        "string",
			"description": "The command to execute (e.g. 'ls -la', 'go test ./...', 'git status')",
		},
	}
	if b.jobs != nil {
		properties["action"] = map[string]any{
			"type":        "string",
			"enum":        []string{"run", "output", "kill"},
			"description": "Action to perform. Defaults to run.",
		}
		properties["background"] = map[string]any{
			"type":        "boolean",
			"description": "For action=run, start the command in the background and return a job id.",
		}
		properties["job_id"] = map[string]any{
			"type":        "string",
			"description": "Background job id for action=output or action=kill.",
		}
		properties["tail_lines"] = map[string]any{
			"type":        "integer",
			"description": "For action=output, return only the last N output lines.",
		}
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

	switch strings.ToLower(strings.TrimSpace(input.Action)) {
	case "", "run":
		if strings.TrimSpace(input.Command) == "" {
			return "", fmt.Errorf("command is required")
		}
		if input.Background {
			if b.jobs == nil {
				return "", fmt.Errorf("background jobs are disabled")
			}
			return b.jobs.start(ctx, b.executor, localCommand{
				CWD:     b.cwd,
				Command: input.Command,
			})
		}
		return b.executor.Run(ctx, localCommand{
			CWD:     b.cwd,
			Command: input.Command,
			Emit:    emit,
		})
	case "output":
		if b.jobs == nil {
			return "", fmt.Errorf("background jobs are disabled")
		}
		return b.jobs.output(input.JobID, input.TailLines)
	case "kill":
		if b.jobs == nil {
			return "", fmt.Errorf("background jobs are disabled")
		}
		return b.jobs.kill(ctx, input.JobID)
	default:
		return "", fmt.Errorf("unsupported bash action %q", input.Action)
	}
}

func (b *Bash) Jobs() []BackgroundJobInfo {
	if b.jobs == nil {
		return nil
	}
	return b.jobs.list()
}

func (b *Bash) StopJob(ctx context.Context, id string) (string, error) {
	if b.jobs == nil {
		return "", fmt.Errorf("background jobs are disabled")
	}
	return b.jobs.kill(ctx, id)
}

func (b *Bash) Close() {
	if b.jobs == nil {
		return
	}
	b.jobs.close()
}
