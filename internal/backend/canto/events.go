package canto

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func (b *Backend) translateEvents(ctx context.Context, evCh <-chan session.Event, turnID uint64) {
	for ev := range evCh {
		if b.translateEvent(ctx, ev, turnID) {
			return
		}
	}
}

func (b *Backend) translateEvent(ctx context.Context, ev session.Event, turnID uint64) bool {
	base := ionEventBase(ev)
	switch ev.Type {
	case session.MessageAdded:
		var msg llm.Message
		if err := ev.UnmarshalData(&msg); err != nil {
			return false
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
			b.events <- ionsession.Error{Base: base, Err: fmt.Errorf("%s", data.Error)}
			b.finishTurn(turnID)
			b.events <- ionsession.TurnFinished{Base: base}
			return true
		}
		b.finishTurn(turnID)
		b.events <- ionsession.TurnFinished{Base: base}
		b.events <- ionsession.StatusChanged{Base: base, Status: "Ready"}
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
			b.markToolComplete(turnID, data.ID)
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
			b.events <- ionsession.ChildDelta{
				Base:      base,
				AgentName: data.ChildID,
				Delta:     data.Message,
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: fmt.Sprintf("Child agent %s: %s", data.ChildID, data.Message)}
		}
	case session.ChildCompleted:
		var data session.ChildCompletedData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildCompleted{
				Base:      base,
				AgentName: data.ChildID,
				Result:    data.Summary,
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: "Ready"}
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
			b.events <- ionsession.StatusChanged{Base: base, Status: "Ready"}
		}
	case session.ChildCanceled:
		var data session.ChildCanceledData
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ChildFailed{
				Base:      base,
				AgentName: data.ChildID,
				Error:     "Canceled: " + data.Reason,
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: "Ready"}
		}
	}
	return false
}

func ionEventBase(ev session.Event) ionsession.Base {
	return ionBaseAt(ev.Timestamp)
}

func ionBaseAt(timestamp time.Time) ionsession.Base {
	if timestamp.IsZero() {
		return ionsession.Base{}
	}
	return ionsession.Base{Timestamp: timestamp.UTC()}
}

func isCancellationTerminal(errText string) bool {
	return strings.Contains(errText, context.Canceled.Error())
}

func isContextOverflowTerminal(errText string) bool {
	errText = strings.ToLower(errText)
	return strings.Contains(errText, "context_length_exceeded") ||
		(strings.Contains(errText, "context") && strings.Contains(errText, "token"))
}
