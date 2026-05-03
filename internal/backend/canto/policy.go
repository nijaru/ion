package canto

import (
	"context"
	"fmt"

	"github.com/nijaru/canto/hook"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	ionsession "github.com/nijaru/ion/internal/session"
)

func policyHook(b *Backend) hook.Handler {
	return hook.FromFunc(
		"ion-policy",
		[]hook.Event{hook.EventPreToolUse},
		func(ctx context.Context, payload *hook.Payload) *hook.Result {
			toolName, _ := payload.Data["tool"].(string)
			args, _ := payload.Data["args"].(string)

			policy, reason := b.policy.Authorize(ctx, toolName, args)
			switch policy {
			case backend.PolicyAllow:
				return &hook.Result{Action: hook.ActionProceed}
			case backend.PolicyDeny:
				return &hook.Result{
					Action: hook.ActionBlock,
					Error:  fmt.Errorf("policy denied: %s", reason),
				}
			case backend.PolicyAsk:
				id := ionsession.ShortID()
				description := fmt.Sprintf("Tool: %s\nArgs: %s\n\n%s", toolName, args, reason)
				environment := ""
				if toolName == "bash" {
					environment = tools.ExecutorEnvironmentSummary()
				}
				ch := b.approver.Request(id)
				defer b.approver.Remove(id)

				b.events <- ionsession.ApprovalRequest{
					RequestID:   id,
					Description: description,
					ToolName:    toolName,
					Args:        args,
					Environment: environment,
				}

				select {
				case <-ctx.Done():
					return &hook.Result{Action: hook.ActionBlock, Error: ctx.Err()}
				case approved := <-ch:
					if !approved {
						return &hook.Result{
							Action: hook.ActionBlock,
							Error:  fmt.Errorf("user denied tool execution"),
						}
					}
					return &hook.Result{Action: hook.ActionProceed}
				}
			default:
				return &hook.Result{Action: hook.ActionProceed}
			}
		},
	)
}

func (b *Backend) Approve(ctx context.Context, requestID string, approved bool) error {
	b.approver.Approve(requestID, approved)
	return nil
}

func (b *Backend) SetMode(mode ionsession.Mode) {
	b.policy.SetMode(mode)
}

func (b *Backend) SetAutoApprove(enabled bool) {
	b.policy.SetAutoApprove(enabled)
}

func (b *Backend) AllowCategory(toolName string) {
	b.policy.AllowCategoryOf(toolName)
}
