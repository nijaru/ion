package main

import (
	"github.com/nijaru/ion/ctxerr"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
	"time"

	"github.com/nijaru/ion/session"
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
	printShort bool,
	promptLong string,
	args []string,
	output string,
	jsonOutput bool,
) (bool, string, string, error) {
	output = strings.ToLower(strings.TrimSpace(output))
	if output == "" {
		output = "text"
	}
	if output != "text" && output != "json" {
		return false, "", "", fmt.Errorf("unsupported print output %q (want text or json)", output)
	}
	if jsonOutput {
		output = "json"
	}

	promptLong = strings.TrimSpace(promptLong)
	prompt := promptLong

	printRequested := printFlag || printShort || prompt != "" || jsonOutput
	if printRequested && prompt == "" && len(args) > 0 {
		prompt = strings.Join(args, " ")
	}
	if printRequested && prompt != "" && len(args) > 0 && promptLong != "" {
		return false, "", "", fmt.Errorf(
			"unexpected arguments after --prompt: %s",
			strings.Join(args, " "),
		)
	}
	if !printRequested && len(args) > 0 {
		return false, "", "", fmt.Errorf("unexpected arguments: %s", strings.Join(args, " "))
	}

	return printRequested, prompt, output, nil
}

// runPrintMode submits a single turn and prints the response to stdout.
func runPrintMode(ctx context.Context, agent session.AgentSession, prompt string) error {
	return runPrintModeWithWriter(ctx, os.Stdout, agent, prompt, "text")
}

func runPrintModeWithWriter(
	ctx context.Context,
	w io.Writer,
	agent session.AgentSession,
	prompt string,
	output string,
) error {
	result, err := runPromptTurn(ctx, agent, prompt)
	if err != nil {
		return err
	}
	return writePrintResult(w, result, output)
}

func runPromptTurn(
	ctx context.Context,
	agent session.AgentSession,
	prompt string,
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
				cancelPrintTurn(agent)
				return printResult{}, fmt.Errorf("event stream closed before turn finished")
			}
			switch msg := ev.(type) {
			case session.ApprovalRequest:
				cancelPrintTurn(agent)
				return printResult{}, fmt.Errorf("unexpected approval request for %s", msg.ToolName)
			case session.ToolCallStart:
				result.ToolCalls = append(result.ToolCalls, msg.ToolName)
			case session.AgentDelta:
				agentText.WriteString(msg.Delta)
			case session.AgentMessage:
				if msg.Message != "" {
					agentText.Reset()
					agentText.WriteString(msg.Message)
				}
				result.InputTokens += msg.InputTokens
				result.OutputTokens += msg.OutputTokens
				result.Cost += msg.Cost
			case session.TurnError:
				cancelPrintTurn(agent)
				if msg.Err == nil {
					return printResult{}, fmt.Errorf("session error")
				}
				return printResult{}, fmt.Errorf("session error: %w", msg.Err)
			case session.TurnEnd:
				seenTurnFinished = true
			}
			if seenTurnFinished {
				result.Response = agentText.String()
				if strings.TrimSpace(result.Response) == "" {
					return printResult{}, fmt.Errorf("turn finished without assistant response")
				}
				return result, nil
			}
		case <-ctx.Done():
			cancelPrintTurn(agent)
			return printResult{}, ctxerr.WrapContext("print turn", ctx.Err())
		}
	}
}

func cancelPrintTurn(agent session.AgentSession) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_ = agent.CancelTurn(ctx)
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

func promptWithStdinContext(prompt, stdinText string) string {
	if prompt == "-" {
		return stdinText
	}
	if prompt == "" {
		return stdinText
	}
	if strings.TrimSpace(stdinText) == "" {
		return prompt
	}
	combined := prompt + "\n\n<stdin>\n" + stdinText
	if !strings.HasSuffix(combined, "\n") {
		combined += "\n"
	}
	combined += "</stdin>"
	return combined
}

// runPrintModeWithTimeout wraps runPrintMode with a configurable timeout.
func runPrintModeWithTimeout(
	ctx context.Context,
	w io.Writer,
	agent session.AgentSession,
	prompt string,
	timeout time.Duration,
	output string,
) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	err := runPrintModeWithWriter(ctx, w, agent, prompt, output)
	if err != nil && errors.Is(err, context.DeadlineExceeded) {
		return ctxerr.Timeout("print mode", timeout, err)
	}
	return err
}

// isStdinPipe returns true if stdin is a pipe (not a terminal).
func isStdinPipe() bool {
	stat, err := os.Stdin.Stat()
	if err != nil {
		return false
	}
	return (stat.Mode() & os.ModeCharDevice) == 0
}
