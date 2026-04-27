package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strings"
	"time"

	"github.com/nijaru/ion/internal/session"
)

type printResult struct {
	SessionID    string   `json:"session_id,omitempty"`
	Response     string   `json:"response"`
	InputTokens  int      `json:"input_tokens,omitempty"`
	OutputTokens int      `json:"output_tokens,omitempty"`
	Cost         float64  `json:"cost,omitempty"`
	ToolCalls    []string `json:"tool_calls,omitempty"`
}

func resolvePrintFlags(
	printFlag bool,
	promptLong string,
	promptShort string,
	args []string,
	output string,
	jsonOutput bool,
) (bool, string, string, error) {
	output = strings.ToLower(strings.TrimSpace(output))
	if output == "" {
		output = "text"
	}
	if jsonOutput {
		if output != "text" && output != "json" {
			return false, "", "", fmt.Errorf("unsupported print output %q (want text or json)", output)
		}
		output = "json"
	}

	promptLong = strings.TrimSpace(promptLong)
	promptShort = strings.TrimSpace(promptShort)
	if promptLong != "" && promptShort != "" {
		return false, "", "", fmt.Errorf("use either -p or --prompt, not both")
	}

	prompt := promptShort
	if prompt == "" {
		prompt = promptLong
	}

	printRequested := printFlag || prompt != "" || jsonOutput
	if printRequested && prompt == "" && len(args) > 0 {
		prompt = strings.Join(args, " ")
	}
	if !printRequested && len(args) > 0 {
		return false, "", "", fmt.Errorf("unexpected arguments: %s", strings.Join(args, " "))
	}

	return printRequested, prompt, output, nil
}

// runPrintMode submits a single turn and prints the response to stdout.
func runPrintMode(ctx context.Context, agent session.AgentSession, prompt string, approveRequests bool) error {
	return runPrintModeWithWriter(ctx, os.Stdout, agent, prompt, approveRequests, "text")
}

func runPrintModeWithWriter(
	ctx context.Context,
	w io.Writer,
	agent session.AgentSession,
	prompt string,
	approveRequests bool,
	output string,
) error {
	result, err := runPromptTurn(ctx, agent, prompt, approveRequests)
	if err != nil {
		return err
	}
	return writePrintResult(w, result, output)
}

func runPromptTurn(
	ctx context.Context,
	agent session.AgentSession,
	prompt string,
	approveRequests bool,
) (printResult, error) {
	if err := agent.SubmitTurn(ctx, prompt); err != nil {
		return printResult{}, fmt.Errorf("submit turn: %w", err)
	}

	var agentText strings.Builder
	result := printResult{SessionID: agent.ID()}
	seenTurnFinished := false

	for {
		select {
		case ev, ok := <-agent.Events():
			if !ok {
				if agentText.Len() > 0 {
					result.Response = agentText.String()
				}
				return result, nil
			}
			switch msg := ev.(type) {
			case session.ApprovalRequest:
				if !approveRequests {
					return printResult{}, fmt.Errorf("approval required for %s", msg.ToolName)
				}
				if err := agent.Approve(ctx, msg.RequestID, true); err != nil {
					return printResult{}, fmt.Errorf("approve %s: %w", msg.ToolName, err)
				}
			case session.ToolCallStarted:
				result.ToolCalls = append(result.ToolCalls, msg.ToolName)
			case session.AgentDelta:
				agentText.WriteString(msg.Delta)
			case session.AgentMessage:
				if msg.Message != "" {
					agentText.Reset()
					agentText.WriteString(msg.Message)
				}
			case session.TokenUsage:
				result.InputTokens += msg.Input
				result.OutputTokens += msg.Output
				result.Cost += msg.Cost
			case session.Error:
				return printResult{}, fmt.Errorf("session error: %w", msg.Err)
			case session.TurnFinished:
				seenTurnFinished = true
			}
			if seenTurnFinished {
				result.Response = agentText.String()
				return result, nil
			}
		case <-ctx.Done():
			return printResult{}, ctx.Err()
		}
	}
}

func writePrintResult(w io.Writer, result printResult, output string) error {
	switch strings.ToLower(strings.TrimSpace(output)) {
	case "", "text":
		_, err := fmt.Fprintln(w, result.Response)
		return err
	case "json":
		enc := json.NewEncoder(w)
		return enc.Encode(result)
	default:
		return fmt.Errorf("unsupported print output %q (want text or json)", output)
	}
}

// runPrintModeWithTimeout wraps runPrintMode with a configurable timeout.
func runPrintModeWithTimeout(
	ctx context.Context,
	w io.Writer,
	agent session.AgentSession,
	prompt string,
	timeout time.Duration,
	approveRequests bool,
	output string,
) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	return runPrintModeWithWriter(ctx, w, agent, prompt, approveRequests, output)
}

// isStdinPipe returns true if stdin is a pipe (not a terminal).
func isStdinPipe() bool {
	stat, err := os.Stdin.Stat()
	if err != nil {
		return false
	}
	return (stat.Mode() & os.ModeCharDevice) == 0
}
