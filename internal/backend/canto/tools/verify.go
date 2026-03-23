package tools

import (
	"context"
	"fmt"
	"os/exec"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

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

	output, err := cmd.CombinedOutput()
	passed := err == nil
	
	outStr := string(output)
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
