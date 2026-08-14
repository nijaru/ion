package tool

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"

	"github.com/nijaru/ion/llm"
)

const (
	SubagentToolName          = "subagent"
	MaxSubagentTaskChars      = 12000
	MaxSubagentToolIterations = 100
	MaxParallelSubagents      = 8
	MaxChainSubagents         = 8
)

// SubagentTaskItem defines one task in a parallel fan-out or sequential chain.
type SubagentTaskItem struct {
	Task  string `json:"task"`
	Model string `json:"model,omitempty"`
}

// SubagentRequest is the bounded delegation request accepted by the built-in
// subagent tool. The runner owns the actual child controller and policy.
type SubagentRequest struct {
	Task              string
	MaxToolIterations int
	Model             string
	Progress          func(string)
}

type SubagentBudget struct {
	MaxToolIterations int    `json:"max_tool_iterations"`
	MaxTokens         int    `json:"max_tokens"`
	MaxOutputChars    int    `json:"max_output_chars"`
	MaxDuration       string `json:"max_duration"`
}

type SubagentResult struct {
	ChildID   string         `json:"child_id"`
	Status    string         `json:"status"`
	Output    string         `json:"output,omitempty"`
	Error     string         `json:"error,omitempty"`
	Budget    SubagentBudget `json:"budget"`
	StartedAt string         `json:"started_at"`
	EndedAt   string         `json:"ended_at"`
}

type SubagentBatchResult struct {
	Mode    string           `json:"mode"`
	Results []SubagentResult `json:"results"`
}

// SubagentRunner is implemented by the runtime controller. It is deliberately
// narrow so the tool package does not own child lifecycle or session state.
type SubagentRunner interface {
	RunSubagent(context.Context, SubagentRequest) (SubagentResult, error)
}

// SubagentTool delegates bounded tasks to runtime-owned child runs.
type SubagentTool struct {
	mu     sync.RWMutex
	runner SubagentRunner
}

func NewSubagentTool() *SubagentTool {
	return &SubagentTool{}
}

// SetRunner installs the runtime owner after host composition has constructed
// the controller. A nil runner fails closed during partial startup.
func (t *SubagentTool) SetRunner(runner SubagentRunner) {
	t.mu.Lock()
	t.runner = runner
	t.mu.Unlock()
}

func (t *SubagentTool) Spec() llm.Spec {
	return llm.Spec{
		Name: SubagentToolName,
		Description: "Delegate tasks to isolated child agents. Supports a single task, parallel tasks, or a sequential chain. " +
			"Child agents execute in their own context and report their findings back for review.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"task": map[string]any{
					"type":        "string",
					"description": "A single self-contained task for the child agent (maximum 12000 characters).",
				},
				"tasks": map[string]any{
					"type": "array",
					"items": map[string]any{
						"type": "object",
						"properties": map[string]any{
							"task": map[string]any{
								"type":        "string",
								"description": "Task for this parallel subagent.",
							},
							"model": map[string]any{
								"type":        "string",
								"description": "Optional model override for this task.",
							},
						},
						"required": []string{"task"},
					},
					"description": "Run up to 8 independent tasks concurrently in parallel.",
				},
				"chain": map[string]any{
					"type": "array",
					"items": map[string]any{
						"type": "object",
						"properties": map[string]any{
							"task": map[string]any{
								"type":        "string",
								"description": "Step task. Use '{previous}' to interpolate output from the prior step.",
							},
							"model": map[string]any{
								"type":        "string",
								"description": "Optional model override for this step.",
							},
						},
						"required": []string{"task"},
					},
					"description": "Run a sequential chain of steps, stopping on the first failure.",
				},
				"max_tool_iterations": map[string]any{
					"type":        "integer",
					"minimum":     0,
					"maximum":     MaxSubagentToolIterations,
					"description": "Maximum child tool iterations (optional; 0 or omitted uses runtime standard policy).",
				},
				"model": map[string]any{
					"type":        "string",
					"description": "Optional model override for the child agent (e.g. a faster or specialized model).",
				},
			},
		},
	}
}

func (t *SubagentTool) Metadata() Metadata {
	return Metadata{Category: "delegation", Concurrency: Parallel}
}

func (t *SubagentTool) ApprovalRequirement(args string) (Requirement, bool, error) {
	input, err := decodeSubagentInput(args)
	if err != nil {
		return Requirement{}, false, err
	}
	resource := input.Task
	if input.Mode == "parallel" {
		resource = fmt.Sprintf("%d parallel tasks", len(input.Tasks))
	} else if input.Mode == "chain" {
		resource = fmt.Sprintf("%d chain steps", len(input.Chain))
	}
	return Requirement{
		Category:  "delegation",
		Operation: "run_subagent",
		Resource:  resource,
		Metadata: map[string]any{
			"mode":                input.Mode,
			"max_tool_iterations": input.MaxToolIterations,
			"recursion":           "disabled",
		},
	}, true, nil
}

func (t *SubagentTool) Execute(ctx context.Context, args string) (string, error) {
	content, _, err := t.ExecuteDetailed(ctx, args)
	return content, err
}

func (t *SubagentTool) ExecuteDetailed(ctx context.Context, args string) (string, any, error) {
	return t.ExecuteDetailedWithProgress(ctx, args, nil)
}

func (t *SubagentTool) ExecuteDetailedWithProgress(
	ctx context.Context,
	args string,
	progress func(StreamUpdate),
) (string, any, error) {
	input, err := decodeSubagentInput(args)
	if err != nil {
		return "", nil, err
	}
	t.mu.RLock()
	runner := t.runner
	t.mu.RUnlock()
	if runner == nil {
		return "", nil, fmt.Errorf("subagent runtime is unavailable")
	}

	switch input.Mode {
	case "parallel":
		return t.executeParallel(ctx, runner, input, progress)
	case "chain":
		return t.executeChain(ctx, runner, input, progress)
	default:
		return t.executeSingle(ctx, runner, input, progress)
	}
}

func (t *SubagentTool) executeSingle(
	ctx context.Context,
	runner SubagentRunner,
	input decodedSubagentInput,
	progress func(StreamUpdate),
) (string, any, error) {
	req := SubagentRequest{
		Task:              input.Task,
		MaxToolIterations: input.MaxToolIterations,
		Model:             input.Model,
	}
	if progress != nil {
		req.Progress = func(text string) {
			if strings.TrimSpace(text) != "" {
				progress(StreamUpdate{Text: text})
			}
		}
	}
	result, runErr := runner.RunSubagent(ctx, req)
	content := result.Output
	if runErr != nil {
		return content, result, runErr
	}
	if result.Error != "" && result.Status != "completed" {
		return content, result, fmt.Errorf("subagent %s: %s", result.Status, result.Error)
	}
	return content, result, nil
}

func (t *SubagentTool) executeParallel(
	ctx context.Context,
	runner SubagentRunner,
	input decodedSubagentInput,
	progress func(StreamUpdate),
) (string, any, error) {
	n := len(input.Tasks)
	results := make([]SubagentResult, n)
	errs := make([]error, n)

	sem := make(chan struct{}, 4)
	var wg sync.WaitGroup
	var progressMu sync.Mutex

	for i, item := range input.Tasks {
		wg.Add(1)
		go func(idx int, taskItem SubagentTaskItem) {
			defer wg.Done()
			select {
			case sem <- struct{}{}:
				defer func() { <-sem }()
			case <-ctx.Done():
				errs[idx] = ctx.Err()
				results[idx] = SubagentResult{Status: "canceled", Error: ctx.Err().Error()}
				return
			}

			model := taskItem.Model
			if model == "" {
				model = input.Model
			}

			req := SubagentRequest{
				Task:              taskItem.Task,
				MaxToolIterations: input.MaxToolIterations,
				Model:             model,
			}
			if progress != nil {
				req.Progress = func(text string) {
					if strings.TrimSpace(text) != "" {
						progressMu.Lock()
						progress(StreamUpdate{Text: fmt.Sprintf("[Task %d] %s", idx+1, text)})
						progressMu.Unlock()
					}
				}
			}

			res, err := runner.RunSubagent(ctx, req)
			results[idx] = res
			errs[idx] = err
		}(i, item)
	}
	wg.Wait()

	var out strings.Builder
	var firstErr error
	for i, res := range results {
		if i > 0 {
			out.WriteString("\n\n")
		}
		taskTitle := input.Tasks[i].Task
		if len([]rune(taskTitle)) > 80 {
			taskTitle = string([]rune(taskTitle)[:80]) + "..."
		}
		fmt.Fprintf(&out, "### Task %d: %s\n%s", i+1, taskTitle, res.Output)
		if errs[i] != nil && firstErr == nil {
			firstErr = errs[i]
		}
	}

	batch := SubagentBatchResult{
		Mode:    "parallel",
		Results: results,
	}
	return out.String(), batch, firstErr
}

func (t *SubagentTool) executeChain(
	ctx context.Context,
	runner SubagentRunner,
	input decodedSubagentInput,
	progress func(StreamUpdate),
) (string, any, error) {
	results := make([]SubagentResult, 0, len(input.Chain))
	prevOutput := ""

	for i, step := range input.Chain {
		if err := ctx.Err(); err != nil {
			return prevOutput, SubagentBatchResult{Mode: "chain", Results: results}, err
		}

		taskText := strings.ReplaceAll(step.Task, "{previous}", prevOutput)
		model := step.Model
		if model == "" {
			model = input.Model
		}

		req := SubagentRequest{
			Task:              taskText,
			MaxToolIterations: input.MaxToolIterations,
			Model:             model,
		}
		if progress != nil {
			req.Progress = func(text string) {
				if strings.TrimSpace(text) != "" {
					progress(StreamUpdate{Text: fmt.Sprintf("[Step %d] %s", i+1, text)})
				}
			}
		}

		res, err := runner.RunSubagent(ctx, req)
		results = append(results, res)
		if err != nil {
			return res.Output, SubagentBatchResult{
					Mode:    "chain",
					Results: results,
				}, fmt.Errorf(
					"step %d failed: %w",
					i+1,
					err,
				)
		}
		if res.Error != "" && res.Status != "completed" {
			return res.Output, SubagentBatchResult{
					Mode:    "chain",
					Results: results,
				}, fmt.Errorf(
					"step %d %s: %s",
					i+1,
					res.Status,
					res.Error,
				)
		}
		prevOutput = res.Output
	}

	var out strings.Builder
	for i, res := range results {
		if i > 0 {
			out.WriteString("\n\n")
		}
		fmt.Fprintf(&out, "### Step %d: %s\n%s", i+1, input.Chain[i].Task, res.Output)
	}

	batch := SubagentBatchResult{
		Mode:    "chain",
		Results: results,
	}
	return out.String(), batch, nil
}

type decodedSubagentInput struct {
	Mode              string
	Task              string
	Tasks             []SubagentTaskItem
	Chain             []SubagentTaskItem
	Model             string
	MaxToolIterations int
}

func decodeSubagentInput(args string) (decodedSubagentInput, error) {
	var raw struct {
		Task              string             `json:"task"`
		Tasks             []SubagentTaskItem `json:"tasks"`
		Chain             []SubagentTaskItem `json:"chain"`
		MaxToolIterations int                `json:"max_tool_iterations"`
		Model             string             `json:"model"`
	}
	if err := json.Unmarshal([]byte(args), &raw); err != nil {
		return decodedSubagentInput{}, fmt.Errorf("decode subagent arguments: %w", err)
	}

	raw.Task = strings.TrimSpace(raw.Task)
	raw.Model = strings.TrimSpace(raw.Model)

	if raw.MaxToolIterations < 0 || raw.MaxToolIterations > MaxSubagentToolIterations {
		return decodedSubagentInput{}, fmt.Errorf(
			"subagent max_tool_iterations must be 0 (default) or between 1 and %d",
			MaxSubagentToolIterations,
		)
	}

	// Determine mode
	hasTask := raw.Task != ""
	hasTasks := len(raw.Tasks) > 0
	hasChain := len(raw.Chain) > 0

	count := 0
	if hasTask {
		count++
	}
	if hasTasks {
		count++
	}
	if hasChain {
		count++
	}

	if count == 0 {
		return decodedSubagentInput{}, fmt.Errorf("subagent requires 'task', 'tasks', or 'chain'")
	}
	if count > 1 {
		return decodedSubagentInput{}, fmt.Errorf("subagent accepts only one of 'task', 'tasks', or 'chain'")
	}

	if hasTask {
		if len([]rune(raw.Task)) > MaxSubagentTaskChars {
			return decodedSubagentInput{}, fmt.Errorf("subagent task exceeds %d characters", MaxSubagentTaskChars)
		}
		return decodedSubagentInput{
			Mode:              "single",
			Task:              raw.Task,
			Model:             raw.Model,
			MaxToolIterations: raw.MaxToolIterations,
		}, nil
	}

	if hasTasks {
		if len(raw.Tasks) > MaxParallelSubagents {
			return decodedSubagentInput{}, fmt.Errorf("maximum %d parallel tasks allowed", MaxParallelSubagents)
		}
		for i, item := range raw.Tasks {
			item.Task = strings.TrimSpace(item.Task)
			if item.Task == "" {
				return decodedSubagentInput{}, fmt.Errorf("tasks[%d].task cannot be empty", i)
			}
			if len([]rune(item.Task)) > MaxSubagentTaskChars {
				return decodedSubagentInput{}, fmt.Errorf(
					"tasks[%d].task exceeds %d characters",
					i,
					MaxSubagentTaskChars,
				)
			}
			raw.Tasks[i] = item
		}
		return decodedSubagentInput{
			Mode:              "parallel",
			Tasks:             raw.Tasks,
			Model:             raw.Model,
			MaxToolIterations: raw.MaxToolIterations,
		}, nil
	}

	if len(raw.Chain) > MaxChainSubagents {
		return decodedSubagentInput{}, fmt.Errorf("maximum %d chain steps allowed", MaxChainSubagents)
	}
	for i, item := range raw.Chain {
		item.Task = strings.TrimSpace(item.Task)
		if item.Task == "" {
			return decodedSubagentInput{}, fmt.Errorf("chain[%d].task cannot be empty", i)
		}
		if len([]rune(item.Task)) > MaxSubagentTaskChars {
			return decodedSubagentInput{}, fmt.Errorf("chain[%d].task exceeds %d characters", i, MaxSubagentTaskChars)
		}
		raw.Chain[i] = item
	}
	return decodedSubagentInput{
		Mode:              "chain",
		Chain:             raw.Chain,
		Model:             raw.Model,
		MaxToolIterations: raw.MaxToolIterations,
	}, nil
}

var (
	_ Tool                      = (*SubagentTool)(nil)
	_ DetailedTool              = (*SubagentTool)(nil)
	_ ProgressAwareDetailedTool = (*SubagentTool)(nil)
	_ MetadataTool              = (*SubagentTool)(nil)
	_ RequirementProvider       = (*SubagentTool)(nil)
)
