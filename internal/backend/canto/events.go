package canto

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func (b *Backend) translateEvent(ctx context.Context, ev session.Event, turnID uint64) bool {
	if !b.acceptsTurnEvent(turnID) {
		return true
	}
	if b.isCancelingTurn(turnID) && ev.Type != session.TurnCompleted {
		return false
	}

	base := ionEventBase(ev)
	switch ev.Type {
	case session.MessageAdded:
		var msg llm.Message
		if err := ev.UnmarshalData(&msg); err != nil {
			return false
		}
		if msg.Role == llm.RoleUser && strings.TrimSpace(msg.Content) != "" {
			b.events <- ionsession.UserMessage{
				Base:    base,
				Message: msg.Content,
			}
		}
		if msg.Role == llm.RoleAssistant &&
			(strings.TrimSpace(msg.Content) != "" || strings.TrimSpace(msg.Reasoning) != "") {
			b.events <- ionsession.AgentMessage{
				Base:      base,
				Message:   msg.Content,
				Reasoning: msg.Reasoning,
			}
		}
	case session.TurnStarted:
		b.events <- ionsession.TurnStarted{Base: base}
		b.events <- ionsession.StatusChanged{Base: base, Status: "Thinking..."}
	case session.TurnCompleted:
		if data, ok, err := ev.TurnCompletedData(); err == nil && ok &&
			data.Error != "" && !isCancellationTerminal(data.Error) {
			if isContextOverflowTerminal(data.Error) {
				return false
			}
			b.emitTurnError(turnID, base, fmt.Errorf("%s", data.Error))
			return true
		}
		b.emitTurnFinished(turnID, base)
		return true
	case session.ToolStarted:
		if data, ok, err := ev.ToolStartedData(); err == nil && ok {
			b.markToolActive(turnID, data.ID)
			b.events <- ionsession.ToolCallStarted{
				Base:      base,
				ToolUseID: data.ID,
				ToolName:  data.Tool,
				Args:      data.Arguments,
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: fmt.Sprintf("Running %s...", data.Tool)}
		}
	case session.ToolCompleted:
		if data, ok, err := ev.ToolCompletedData(); err == nil && ok {
			tracked, remaining := b.markToolComplete(turnID, data.ID)
			var execErr error
			if data.Error != "" {
				execErr = fmt.Errorf("%s", data.Error)
			}
			b.events <- ionsession.ToolResult{
				Base:      base,
				ToolUseID: data.ID,
				ToolName:  data.Tool,
				Result:    data.Output,
				Error:     execErr,
			}
			if tracked && !remaining {
				b.events <- ionsession.StatusChanged{Base: base, Status: "Thinking..."}
			}
		}
	case session.ToolOutputDelta:
		var data struct {
			ID    string `json:"id"`
			Delta string `json:"delta"`
		}
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ToolOutputDelta{
				Base:      base,
				ToolUseID: data.ID,
				Delta:     data.Delta,
			}
		}
	case session.ChildRequested:
		var data session.ChildRequestedData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildRequested{
				Base:      base,
				AgentName: data.ChildID,
				Query:     data.Task,
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: fmt.Sprintf("Requesting child agent %s...", data.ChildID)}
		}
	case session.ChildStarted:
		var data session.ChildStartedData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildStarted{
				Base:      base,
				AgentName: data.ChildID,
				SessionID: data.ChildSessionID,
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: fmt.Sprintf("Child agent %s started (%s)", data.ChildID, data.ChildSessionID)}
		}
	case session.ChildProgressed:
		var data session.ChildProgressedData
		if err := ev.UnmarshalData(&data); err == nil {
			message := strings.TrimSpace(data.Message)
			status := message
			if status == "" {
				status = strings.TrimSpace(data.Status)
			}
			if message != "" {
				b.events <- ionsession.ChildDelta{
					Base:      base,
					AgentName: data.ChildID,
					Delta:     data.Message,
				}
			}
			if status != "" {
				b.events <- ionsession.StatusChanged{
					Base:   base,
					Status: fmt.Sprintf("Child agent %s: %s", data.ChildID, status),
				}
			}
		}
	case session.ChildCompleted:
		var data session.ChildCompletedData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildCompleted{
				Base:      base,
				AgentName: data.ChildID,
				Result:    data.Summary,
			}
			if usage, ok := tokenUsageFromCantoUsage(data.Usage); ok {
				usage.Base = base
				b.events <- usage
			}
		}
	case session.ChildBlocked:
		var data session.ChildBlockedData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildBlocked{
				Base:      base,
				AgentName: data.ChildID,
				Reason:    data.Reason,
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: fmt.Sprintf("Child agent %s blocked", data.ChildID)}
		}
	case session.ChildFailed:
		var data session.ChildFailedData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildFailed{
				Base:      base,
				AgentName: data.ChildID,
				Error:     data.Error,
			}
		}
	case session.ChildCanceled:
		var data session.ChildCanceledData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildCanceled{
				Base:      base,
				AgentName: data.ChildID,
				Reason:    data.Reason,
			}
		}
	}
	return false
}

func ionEventBase(ev session.Event) ionsession.Base {
	return ionsession.BaseAt(ev.Timestamp)
}

func isCancellationTerminal(errText string) bool {
	return strings.Contains(errText, context.Canceled.Error())
}

func isContextOverflowTerminal(errText string) bool {
	errText = strings.ToLower(errText)
	return strings.Contains(errText, "context_length_exceeded") ||
		(strings.Contains(errText, "context") && strings.Contains(errText, "token"))
}
