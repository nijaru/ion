package main

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/config"
	session "github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

type acpApprovalSession interface {
	Approve(ctx context.Context, requestID string, approved bool) error
}

func (a *ionACPAgent) forwardEvent(
	ctx context.Context,
	sessionID string,
	sess *ionACPHeadlessSession,
	event session.AgentEvent,
) (bool, acp.StopReason, error) {
	switch e := event.(type) {
	case session.TurnStart:
		return false, "", nil
	case session.TurnEnd:
		return true, acp.StopReasonEndTurn, nil
	case session.AgentDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentMessageText(e.Delta))
	case session.ThinkingDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentThoughtText(e.Delta))
	case session.AgentMessage:
		if e.Reasoning != "" {
			if err := a.sessionUpdate(ctx, sessionID, acp.UpdateAgentThoughtText(e.Reasoning)); err != nil {
				return false, "", err
			}
		}
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentMessageText(e.Message))
	case session.ToolCallStart:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolCallStart(sess.cwd, e))
	case session.ToolOutputDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolOutputDelta(e))
	case session.ToolCallEnd:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolCallResult(e))
	case session.ApprovalRequest:
		if err := a.requestPermission(ctx, sessionID, sess, e); err != nil {
			return false, "", err
		}
		return false, "", nil
	case session.TurnError:
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
	req session.ApprovalRequest,
) error {
	approvalSession, ok := sess.agent.(acpApprovalSession)
	if !ok {
		return fmt.Errorf("session does not support approval requests")
	}
	if a.conn == nil {
		return approvalSession.Approve(ctx, req.RequestID, false)
	}
	title := config.Redact(tool.Title(req.ToolName, req.Args, tool.Options{
		Workdir: sess.cwd,
		Width:   100,
	}))
	redactedArgs := config.Redact(req.Args)
	kind := acpToolKind(req.ToolName)
	status := acp.ToolCallStatusPending
	resp, err := a.conn.RequestPermission(ctx, acp.RequestPermissionRequest{
		SessionId: acp.SessionId(sessionID),
		ToolCall: acp.ToolCallUpdate{
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
	return approvalSession.Approve(ctx, req.RequestID, approved)
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

func acpToolCallStart(workdir string, e session.ToolCallStart) acp.SessionUpdate {
	title := config.Redact(tool.Title(e.ToolName, e.Args, tool.Options{
		Workdir: workdir,
		Width:   100,
	}))
	redactedArgs := config.Redact(e.Args)
	return acp.StartToolCall(
		acp.ToolCallId(e.ToolUseID),
		title,
		acp.WithStartKind(acpToolKind(e.ToolName)),
		acp.WithStartStatus(acp.ToolCallStatusPending),
		acp.WithStartRawInput(acpRawInput(redactedArgs)),
		acp.WithStartLocations(acpLocations(e.Args)),
	)
}

func acpToolOutputDelta(e session.ToolOutputDelta) acp.SessionUpdate {
	delta := config.Redact(e.Delta)
	return acp.UpdateToolCall(
		acp.ToolCallId(e.ToolUseID),
		acp.WithUpdateStatus(acp.ToolCallStatusInProgress),
		acp.WithUpdateContent([]acp.ToolCallContent{
			acp.ToolContent(acp.TextBlock(delta)),
		}),
	)
}

func acpToolCallResult(e session.ToolCallEnd) acp.SessionUpdate {
	status := acp.ToolCallStatusCompleted
	output := e.Result
	if e.Error != nil {
		status = acp.ToolCallStatusFailed
		if output == "" {
			output = e.Error.Error()
		}
	}
	output = config.Redact(output)
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
	case "write", "edit":
		return acp.ToolKindEdit
	case "list", "ls", "grep", "glob", "find":
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
