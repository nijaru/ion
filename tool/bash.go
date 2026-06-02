package tool

import (
	"context"
	"errors"
	"fmt"
	"iter"
	"strings"
	"time"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/ion/llm"
)

type Bash struct {
	cwd      string
	executor *localExecutor
}

var (
	_ StreamingTool       = (*Bash)(nil)
	_ StreamingUpdateTool = (*Bash)(nil)
)

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
		executor: newLocalExecutorWithEnvironment(SandboxOff, environment),
	}
}

func (b *Bash) Spec() llm.Spec {
	properties := map[string]any{
		"command": map[string]any{
			"type":        "string",
			"description": "The command to execute (e.g. 'ls -la', 'go test ./...', 'git status')",
		},
		"timeout": map[string]any{
			"type":        "number",
			"description": "Timeout in seconds (optional, no default timeout).",
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
		for update, err := range b.ExecuteStreamingUpdates(ctx, args) {
			if err != nil {
				if !yield("", err) {
					return
				}
				return
			}
			if !yield(update.Text, nil) {
				return
			}
		}
	}
}

func (b *Bash) ExecuteStreamingUpdates(
	ctx context.Context,
	args string,
) iter.Seq2[StreamUpdate, error] {
	return func(yield func(StreamUpdate, error) bool) {
		streamCtx, cancel := context.WithCancel(ctx)
		defer cancel()

		type streamItem struct {
			update StreamUpdate
			err    error
		}
		ch := make(chan streamItem, 16)

		go func() {
			_, err := b.execute(streamCtx, args, func(update localOutputUpdate) error {
				select {
				case ch <- streamItem{update: StreamUpdate{
					Text:     update.Text,
					Snapshot: update.Snapshot,
				}}:
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
			if !yield(item.update, item.err) {
				cancel()
				return
			}
		}
	}
}

func (b *Bash) execute(
	ctx context.Context,
	args string,
	emit func(localOutputUpdate) error,
) (string, error) {
	input, err := parseBashInput(args)
	if err != nil {
		return "", err
	}

	runCtx := ctx
	var cancel context.CancelFunc
	if input.Timeout > 0 {
		timeout := time.Duration(input.Timeout * float64(time.Second))
		if timeout <= 0 {
			return "", fmt.Errorf("timeout is too large")
		}
		runCtx, cancel = context.WithTimeout(ctx, timeout)
		defer cancel()
	}

	result, err := b.executor.Run(runCtx, localCommand{
		CWD:     b.cwd,
		Command: input.Command,
		Emit:    emit,
	})
	if input.Timeout > 0 && errors.Is(runCtx.Err(), context.DeadlineExceeded) {
		return result, fmt.Errorf("timeout after %.3g seconds", input.Timeout)
	}
	if ctxErr := runCtx.Err(); ctxErr != nil {
		return result, toolContextErr("bash", ctxErr)
	}
	return result, err
}

type bashInput struct {
	Command    string  `json:"command"`
	Timeout    float64 `json:"timeout"`
	Action     string  `json:"action"`
	Background bool    `json:"background"`
	JobID      string  `json:"job_id"`
	TailLines  int     `json:"tail_lines"`
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
