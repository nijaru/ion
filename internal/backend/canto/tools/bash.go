package tools

import (
	"context"
	"errors"
	"fmt"
	"iter"
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/tool"
)

type Bash struct {
	cwd      string
	executor *localExecutor
}

var _ tool.StreamingTool = (*Bash)(nil)

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
			"required":   []string{"command"},
		},
	}
}

func (b *Bash) Execute(ctx context.Context, args string) (string, error) {
	return b.execute(ctx, args, nil)
}

func (b *Bash) ExecuteStreaming(ctx context.Context, args string) iter.Seq2[string, error] {
	return func(yield func(string, error) bool) {
		streamCtx, cancel := context.WithCancel(ctx)
		defer cancel()

		type streamItem struct {
			text string
			err  error
		}
		ch := make(chan streamItem, 16)

		go func() {
			_, err := b.execute(streamCtx, args, func(chunk string) error {
				select {
				case ch <- streamItem{text: chunk}:
					return nil
				case <-streamCtx.Done():
					return streamCtx.Err()
				}
			})
			if err != nil && !errors.Is(err, context.Canceled) {
				select {
				case ch <- streamItem{err: err}:
				case <-streamCtx.Done():
				}
			}
			close(ch)
		}()

		for item := range ch {
			if !yield(item.text, item.err) {
				cancel()
				return
			}
		}
	}
}

func (b *Bash) execute(
	ctx context.Context,
	args string,
	emit func(string) error,
) (string, error) {
	input, err := parseBashInput(args)
	if err != nil {
		return "", err
	}
	return b.executor.Run(ctx, localCommand{
		CWD:     b.cwd,
		Command: input.Command,
		Emit:    emit,
	})
}

type bashInput struct {
	Command    string `json:"command"`
	Action     string `json:"action"`
	Background bool   `json:"background"`
	JobID      string `json:"job_id"`
	TailLines  int    `json:"tail_lines"`
}

func parseBashInput(args string) (bashInput, error) {
	var input bashInput
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return bashInput{}, err
	}

	action := strings.ToLower(strings.TrimSpace(input.Action))
	if action != "" && action != "run" ||
		input.Background ||
		strings.TrimSpace(input.JobID) != "" ||
		input.TailLines != 0 {
		return bashInput{}, fmt.Errorf("background jobs are deferred")
	}
	if strings.TrimSpace(input.Command) == "" {
		return bashInput{}, fmt.Errorf("command is required")
	}
	return input, nil
}
