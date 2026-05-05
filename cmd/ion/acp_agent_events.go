package main

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/internal/privacy"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/tooldisplay"
)

func (a *ionACPAgent) forwardEvent(
	ctx context.Context,
	sessionID string,
	sess *ionACPHeadlessSession,
	event ionsession.Event,
) (bool, acp.StopReason, error) {
	switch e := event.(type) {
	case ionsession.TurnStarted:
		return false, "", nil
	case ionsession.TurnFinished:
		return true, acp.StopReasonEndTurn, nil
	case ionsession.AgentDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentMessageText(e.Delta))
	case ionsession.ThinkingDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentThoughtText(e.Delta))
	case ionsession.AgentMessage:
		if e.Reasoning != "" {
			if err := a.sessionUpdate(ctx, sessionID, acp.UpdateAgentThoughtText(e.Reasoning)); err != nil {
				return false, "", err
			}
		}
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentMessageText(e.Message))
	case ionsession.ToolCallStarted:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolCallStart(sess.cwd, e))
	case ionsession.ToolOutputDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolOutputDelta(e))
	case ionsession.ToolResult:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolCallResult(e))
	case ionsession.ApprovalRequest:
		if err := a.requestPermission(ctx, sessionID, sess, e); err != nil {
			return false, "", err
		}
		return false, "", nil
	case ionsession.Error:
		if e.Err != nil {
			return false, "", e.Err
		}
		return false, "", fmt.Errorf("session error")
	default:
		return false, "", nil
	}
}

func (a *ionACPAgent) sessionUpdate(
	ctx context.Context,
	sessionID string,
	update acp.SessionUpdate,
) error {
	if a.conn == nil {
		return nil
	}
	return a.conn.SessionUpdate(ctx, acp.SessionNotification{
		SessionId: acp.SessionId(sessionID),
		Update:    update,
	})
}

func (a *ionACPAgent) requestPermission(
	ctx context.Context,
	sessionID string,
	sess *ionACPHeadlessSession,
	req ionsession.ApprovalRequest,
) error {
	if a.conn == nil {
		return sess.agent.Approve(ctx, req.RequestID, false)
	}
	title := privacy.Redact(tooldisplay.Title(req.ToolName, req.Args, tooldisplay.Options{
		Workdir: sess.cwd,
		Width:   100,
	}))
	redactedArgs := privacy.Redact(req.Args)
	kind := acpToolKind(req.ToolName)
	status := acp.ToolCallStatusPending
	resp, err := a.conn.RequestPermission(ctx, acp.RequestPermissionRequest{
		SessionId: acp.SessionId(sessionID),
		ToolCall: acp.RequestPermissionToolCall{
			ToolCallId: acp.ToolCallId(req.RequestID),
			Title:      &title,
			Kind:       &kind,
			Status:     &status,
			RawInput:   acpRawInput(redactedArgs),
			Locations:  acpLocations(req.Args),
		},
		Options: []acp.PermissionOption{
			{
				Kind:     acp.PermissionOptionKindAllowOnce,
				Name:     "Allow once",
				OptionId: acp.PermissionOptionId("allow"),
			},
			{
				Kind:     acp.PermissionOptionKindRejectOnce,
				Name:     "Reject",
				OptionId: acp.PermissionOptionId("reject"),
			},
		},
	})
	if err != nil {
		return err
	}
	approved := resp.Outcome.Selected != nil &&
		string(resp.Outcome.Selected.OptionId) == "allow"
	return sess.agent.Approve(ctx, req.RequestID, approved)
}

func acpPromptText(blocks []acp.ContentBlock) (string, error) {
	var b strings.Builder
	for _, block := range blocks {
		switch {
		case block.Text != nil:
			b.WriteString(block.Text.Text)
		case block.ResourceLink != nil:
			if b.Len() > 0 {
				b.WriteString("\n\n")
			}
			b.WriteString(block.ResourceLink.Name)
			b.WriteString(": ")
			b.WriteString(block.ResourceLink.Uri)
		default:
			return "", fmt.Errorf("unsupported ACP prompt content block")
		}
	}
	return strings.TrimSpace(b.String()), nil
}

func acpToolCallStart(workdir string, e ionsession.ToolCallStarted) acp.SessionUpdate {
	title := privacy.Redact(tooldisplay.Title(e.ToolName, e.Args, tooldisplay.Options{
		Workdir: workdir,
		Width:   100,
	}))
	redactedArgs := privacy.Redact(e.Args)
	return acp.StartToolCall(
		acp.ToolCallId(e.ToolUseID),
		title,
		acp.WithStartKind(acpToolKind(e.ToolName)),
		acp.WithStartStatus(acp.ToolCallStatusPending),
		acp.WithStartRawInput(acpRawInput(redactedArgs)),
		acp.WithStartLocations(acpLocations(e.Args)),
	)
}

func acpToolOutputDelta(e ionsession.ToolOutputDelta) acp.SessionUpdate {
	delta := privacy.Redact(e.Delta)
	return acp.UpdateToolCall(
		acp.ToolCallId(e.ToolUseID),
		acp.WithUpdateStatus(acp.ToolCallStatusInProgress),
		acp.WithUpdateContent([]acp.ToolCallContent{
			acp.ToolContent(acp.TextBlock(delta)),
		}),
	)
}

func acpToolCallResult(e ionsession.ToolResult) acp.SessionUpdate {
	status := acp.ToolCallStatusCompleted
	output := e.Result
	if e.Error != nil {
		status = acp.ToolCallStatusFailed
		if output == "" {
			output = e.Error.Error()
		}
	}
	output = privacy.Redact(output)
	return acp.UpdateToolCall(
		acp.ToolCallId(e.ToolUseID),
		acp.WithUpdateStatus(status),
		acp.WithUpdateRawOutput(output),
		acp.WithUpdateContent([]acp.ToolCallContent{
			acp.ToolContent(acp.TextBlock(output)),
		}),
	)
}

func acpToolKind(name string) acp.ToolKind {
	switch strings.ToLower(strings.TrimSpace(name)) {
	case "read":
		return acp.ToolKindRead
	case "write", "edit", "multi_edit":
		return acp.ToolKindEdit
	case "list", "grep", "glob":
		return acp.ToolKindSearch
	case "bash":
		return acp.ToolKindExecute
	default:
		return acp.ToolKindOther
	}
}

func acpRawInput(args string) any {
	args = strings.TrimSpace(args)
	if args == "" {
		return nil
	}
	var value any
	if err := json.Unmarshal([]byte(args), &value); err == nil {
		return value
	}
	return args
}

func acpLocations(args string) []acp.ToolCallLocation {
	var raw map[string]any
	if err := json.Unmarshal([]byte(args), &raw); err != nil {
		return nil
	}
	for _, key := range []string{"file_path", "path"} {
		value, ok := raw[key].(string)
		if ok && strings.TrimSpace(value) != "" {
			return []acp.ToolCallLocation{{Path: value}}
		}
	}
	return nil
}
