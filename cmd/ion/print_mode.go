package main

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/nijaru/ion/internal/session"
)

// runPrintMode submits a single turn and prints the response to stdout.
func runPrintMode(ctx context.Context, agent session.AgentSession, prompt string, approveRequests bool) error {
	if err := agent.SubmitTurn(ctx, prompt); err != nil {
		return fmt.Errorf("submit turn: %w", err)
	}

	var agentText strings.Builder
	seenTurnFinished := false

	for {
		select {
		case ev, ok := <-agent.Events():
			if !ok {
				if agentText.Len() > 0 {
					fmt.Println(agentText.String())
				}
				return nil
			}
			switch msg := ev.(type) {
			case session.ApprovalRequest:
				if !approveRequests {
					return fmt.Errorf("approval required for %s", msg.ToolName)
				}
				if err := agent.Approve(ctx, msg.RequestID, true); err != nil {
					return fmt.Errorf("approve %s: %w", msg.ToolName, err)
				}
			case session.AgentDelta:
				agentText.WriteString(msg.Delta)
			case session.AgentMessage:
				if msg.Message != "" {
					agentText.Reset()
					agentText.WriteString(msg.Message)
				}
			case session.Error:
				return fmt.Errorf("session error: %w", msg.Err)
			case session.TurnFinished:
				seenTurnFinished = true
			}
			if seenTurnFinished {
				fmt.Println(agentText.String())
				return nil
			}
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

// runPrintModeWithTimeout wraps runPrintMode with a configurable timeout.
func runPrintModeWithTimeout(ctx context.Context, agent session.AgentSession, prompt string, timeout time.Duration, approveRequests bool) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	return runPrintMode(ctx, agent, prompt, approveRequests)
}

// isStdinPipe returns true if stdin is a pipe (not a terminal).
func isStdinPipe() bool {
	stat, err := os.Stdin.Stat()
	if err != nil {
		return false
	}
	return (stat.Mode() & os.ModeCharDevice) == 0
}
