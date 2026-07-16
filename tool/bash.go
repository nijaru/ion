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
	jobs     *JobManager
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
	return NewBashWithEnvironmentAndJobs(cwd, environment, nil)
}

// NewBashWithEnvironmentAndJobs creates a Bash tool with an optional runtime
// job manager. A nil manager keeps the foreground-only tool useful in focused
// library callers and tests.
func NewBashWithEnvironmentAndJobs(
	cwd string,
	environment EnvironmentPolicy,
	jobs *JobManager,
) *Bash {
	return &Bash{
		cwd:      cwd,
		executor: newLocalExecutorWithEnvironment(resolveSandboxMode(), environment),
		jobs:     jobs,
	}
}

func (b *Bash) Spec() llm.Spec {
	properties := map[string]any{
		"command": map[string]any{
			"type":        "string",
			"description": "The command to execute for action=run (e.g. 'ls -la', 'go test ./...', 'git status').",
		},
		"timeout": map[string]any{
			"type":        "number",
			"description": "Timeout in seconds (optional, no default timeout).",
		},
		"action": map[string]any{
			"type":        "string",
			"enum":        []string{"run", "list", "output", "stop"},
			"description": "Job operation. Omit or use run for a command; use list, output, or stop for managed background jobs.",
		},
		"background": map[string]any{
			"type":        "boolean",
			"description": "For action=run, start the command as a managed background job and return its job_id.",
		},
		"job_id": map[string]any{
			"type":        "string",
			"description": "Managed job ID for action=output or action=stop.",
		},
		"tail_lines": map[string]any{
			"type":        "integer",
			"minimum":     1,
			"description": "Maximum output lines to return for action=output; defaults to 50.",
		},
	}

	return llm.Spec{
		Name:        "bash",
		Description: "Run a shell command in the current working directory, or manage an explicitly requested background job. Always prefer non-interactive commands (e.g. use --yes flags) to prevent hanging the TUI.",
		Parameters: map[string]any{
			"type":       "object",
			"properties": properties,
		},
	}
}

// ApprovalRequirement treats every foreground/background shell command as a
// potentially mutating operation. Job inspection and cancellation are
// runtime management, not shell execution, and do not require approval.
func (b *Bash) ApprovalRequirement(args string) (Requirement, bool, error) {
	input, err := parseBashInput(args)
	if err != nil {
		return Requirement{}, false, err
	}
	if input.Action != "run" {
		return Requirement{}, false, nil
	}
	return Requirement{
		Category:  "execute",
		Operation: "bash",
		Resource:  input.Command,
	}, true, nil
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
				case ch <- streamItem{update: StreamUpdate(update)}:
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
	if input.Action != "run" {
		return b.executeJobAction(input)
	}
	if input.Background {
		if b.jobs == nil {
			return "", errors.New("background jobs require a runtime job manager")
		}
		id, err := b.jobs.start(ctx, input.Command, func(
			jobCtx context.Context,
			started func(int),
			emit func(localOutputUpdate) error,
		) (string, error) {
			return b.runCommand(jobCtx, input, started, emit, false)
		})
		if err != nil {
			return "", err
		}
		return fmt.Sprintf("background job %s started", id), nil
	}
	return b.runCommand(ctx, input, nil, emit, true)
}

func (b *Bash) runCommand(
	ctx context.Context,
	input bashInput,
	started func(int),
	emit func(localOutputUpdate) error,
	persistFullOutput bool,
) (string, error) {

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
		CWD:               b.cwd,
		Command:           input.Command,
		Emit:              emit,
		Started:           started,
		PersistFullOutput: persistFullOutput,
	})
	if input.Timeout > 0 && errors.Is(runCtx.Err(), context.DeadlineExceeded) {
		return result, fmt.Errorf("timeout after %.3g seconds", input.Timeout)
	}
	if ctxErr := runCtx.Err(); ctxErr != nil {
		return result, toolContextErr("bash", ctxErr)
	}
	return result, err
}

func (b *Bash) executeJobAction(input bashInput) (string, error) {
	if b.jobs == nil {
		return "", errors.New("background jobs require a runtime job manager")
	}
	switch input.Action {
	case "list":
		jobs := b.jobs.List()
		if len(jobs) == 0 {
			return "no background jobs", nil
		}
		var output strings.Builder
		for _, job := range jobs {
			fmt.Fprintf(&output, "%s\t%s\t%s", job.ID, job.Status, job.Command)
			if job.Error != "" {
				fmt.Fprintf(&output, "\terror: %s", job.Error)
			}
			output.WriteByte('\n')
		}
		return strings.TrimRight(output.String(), "\n"), nil
	case "output":
		job, err := b.jobs.Get(input.JobID)
		if err != nil {
			return "", err
		}
		return formatJobOutput(job, input.TailLines), nil
	case "stop":
		if err := b.jobs.Stop(input.JobID); err != nil {
			return "", err
		}
		return fmt.Sprintf("background job %s stopped", input.JobID), nil
	default:
		return "", fmt.Errorf("unsupported bash action %q", input.Action)
	}
}

func formatJobOutput(job JobSnapshot, tailLines int) string {
	if tailLines <= 0 {
		tailLines = 50
	}
	output := job.Output
	if lines := strings.Split(output, "\n"); len(lines) > tailLines {
		output = strings.Join(lines[len(lines)-tailLines:], "\n")
	}
	var result strings.Builder
	fmt.Fprintf(&result, "job %s\nstatus: %s\ncommand: %s", job.ID, job.Status, job.Command)
	if job.Error != "" {
		fmt.Fprintf(&result, "\nerror: %s", job.Error)
	}
	if output != "" {
		result.WriteString("\noutput:\n")
		result.WriteString(output)
	}
	return result.String()
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
	if input.Timeout < 0 {
		return bashInput{}, fmt.Errorf("timeout must be non-negative")
	}

	input.Action = strings.ToLower(strings.TrimSpace(input.Action))
	if input.Action == "" {
		input.Action = "run"
	}
	if input.TailLines < 0 {
		return bashInput{}, fmt.Errorf("tail_lines must be positive")
	}
	switch input.Action {
	case "run":
		if strings.TrimSpace(input.JobID) != "" || input.TailLines != 0 {
			return bashInput{}, fmt.Errorf("job_id and tail_lines require action=output or action=stop")
		}
		if strings.TrimSpace(input.Command) == "" {
			return bashInput{}, fmt.Errorf("command is required")
		}
	case "list":
		if input.Background || strings.TrimSpace(input.Command) != "" ||
			strings.TrimSpace(input.JobID) != "" || input.TailLines != 0 || input.Timeout != 0 {
			return bashInput{}, fmt.Errorf("action=list does not accept command, timeout, background, job_id, or tail_lines")
		}
	case "output":
		if input.Background || strings.TrimSpace(input.Command) != "" || input.Timeout != 0 ||
			strings.TrimSpace(input.JobID) == "" {
			return bashInput{}, fmt.Errorf("action=output requires job_id and does not accept command, timeout, or background")
		}
	case "stop":
		if input.Background || strings.TrimSpace(input.Command) != "" || input.Timeout != 0 ||
			strings.TrimSpace(input.JobID) == "" || input.TailLines != 0 {
			return bashInput{}, fmt.Errorf("action=stop requires job_id and does not accept command, timeout, background, or tail_lines")
		}
	default:
		return bashInput{}, fmt.Errorf("unsupported action %q", input.Action)
	}
	return input, nil
}
