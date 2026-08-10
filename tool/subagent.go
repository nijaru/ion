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
	MaxSubagentToolIterations = 16
)

// SubagentRequest is the bounded delegation request accepted by the built-in
// subagent tool. The runner owns the actual child controller and policy.
type SubagentRequest struct {
	Task              string
	MaxToolIterations int
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

// SubagentRunner is implemented by the runtime controller. It is deliberately
// narrow so the tool package does not own child lifecycle or session state.
type SubagentRunner interface {
	RunSubagent(context.Context, SubagentRequest) (SubagentResult, error)
}

// SubagentTool delegates one bounded task to a runtime-owned child run.
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
		Name:        SubagentToolName,
		Description: "Delegate one bounded coding task to an isolated child agent. The child cannot delegate again; its result is returned for review.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"task": map[string]any{
					"type":        "string",
					"description": "A self-contained task for the child agent, maximum 12000 characters.",
				},
				"max_tool_iterations": map[string]any{
					"type":        "integer",
					"minimum":     0,
					"maximum":     MaxSubagentToolIterations,
					"description": "Maximum child tool iterations, default is runtime policy.",
				},
			},
			"required": []string{"task"},
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
	return Requirement{
		Category:  "delegation",
		Operation: "run_subagent",
		Resource:  input.Task,
		Metadata: map[string]any{
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
	if progress != nil {
		input.Progress = func(text string) {
			if strings.TrimSpace(text) != "" {
				progress(StreamUpdate{Text: text})
			}
		}
	}
	result, runErr := runner.RunSubagent(ctx, input)
	content := result.Output
	if runErr != nil {
		return content, result, runErr
	}
	if result.Error != "" && result.Status != "completed" {
		return content, result, fmt.Errorf("subagent %s: %s", result.Status, result.Error)
	}
	return content, result, nil
}

func decodeSubagentInput(args string) (SubagentRequest, error) {
	var input struct {
		Task              string `json:"task"`
		MaxToolIterations int    `json:"max_tool_iterations"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return SubagentRequest{}, fmt.Errorf("decode subagent arguments: %w", err)
	}
	input.Task = strings.TrimSpace(input.Task)
	if input.Task == "" {
		return SubagentRequest{}, fmt.Errorf("subagent task is required")
	}
	if len([]rune(input.Task)) > MaxSubagentTaskChars {
		return SubagentRequest{}, fmt.Errorf("subagent task exceeds %d characters", MaxSubagentTaskChars)
	}
	if input.MaxToolIterations < 0 || input.MaxToolIterations > MaxSubagentToolIterations {
		return SubagentRequest{}, fmt.Errorf(
			"subagent max_tool_iterations must be 0 (default) or between 1 and %d",
			MaxSubagentToolIterations,
		)
	}
	return SubagentRequest{Task: input.Task, MaxToolIterations: input.MaxToolIterations}, nil
}

var (
	_ Tool                      = (*SubagentTool)(nil)
	_ DetailedTool              = (*SubagentTool)(nil)
	_ ProgressAwareDetailedTool = (*SubagentTool)(nil)
	_ MetadataTool              = (*SubagentTool)(nil)
	_ RequirementProvider       = (*SubagentTool)(nil)
)
