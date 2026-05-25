package canto

import (
	"context"
	"fmt"
	"strings"
	"time"

	cantofw "github.com/nijaru/canto"
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
		if turnID != 0 {
			return false
		}
		if data, ok, err := ev.TurnCompletedData(); err == nil && ok &&
			data.Error != "" && !isCancellationTerminal(data.Error) {
			if isContextOverflowTerminal(data.Error) {
				return false
			}
			if turnID != 0 {
				b.emitTurnErrorOnce(turnID, base, fmt.Errorf("%s", data.Error))
				return true
			}
			b.emitTurnError(turnID, base, fmt.Errorf("%s", data.Error))
			return true
		}
		if turnID != 0 {
			b.emitTurnTerminal(turnID, base)
			return true
		}
		b.emitTurnFinished(turnID, base)
		return true
	case session.ToolStarted:
		if data, ok, err := ev.ToolStartedData(); err == nil && ok {
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
			ID       string `json:"id"`
			Delta    string `json:"delta"`
			Snapshot bool   `json:"snapshot"`
		}
		if err := ev.UnmarshalData(&data); err == nil {
			b.events <- ionsession.ToolOutputDelta{
				Base:      base,
				ToolUseID: data.ID,
				Delta:     data.Delta,
				Snapshot:  data.Snapshot,
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

func runEventBase(event cantofw.RunEvent) ionsession.Base {
	if sessionEvent, ok := event.SessionEvent(); ok && !sessionEvent.Timestamp.IsZero() {
		return ionEventBase(sessionEvent)
	}
	return ionsession.BaseNow()
}

func isCancellationTerminal(errText string) bool {
	return strings.Contains(errText, context.Canceled.Error())
}

func isContextOverflowTerminal(errText string) bool {
	errText = strings.ToLower(errText)
	return strings.Contains(errText, "context_length_exceeded") ||
		(strings.Contains(errText, "context") && strings.Contains(errText, "token"))
}

func (b *Backend) translateRunSessionEvent(
	ctx context.Context,
	event cantofw.RunEvent,
	turnID uint64,
) bool {
	ev, hasSessionEvent := event.SessionEvent()
	lifecycle := event.Lifecycle
	if lifecycle == nil {
		if hasSessionEvent {
			return b.translateEvent(ctx, ev, turnID)
		}
		return false
	}
	if b.isCancelingTurn(turnID) && (!hasSessionEvent || ev.Type != session.TurnCompleted) {
		return false
	}

	base := runEventBase(event)
	switch lifecycle.Type {
	case cantofw.RunLifecycleTurn:
		if !lifecycle.Terminal {
			break
		}
		b.emitRunUsage(base, lifecycle.Usage)
		if turnID != 0 {
			return false
		}
		if lifecycle.Status == cantofw.RunLifecycleFailed &&
			!lifecycle.Canceled &&
			!isCancellationTerminal(lifecycle.Error) {
			if isContextOverflowTerminal(lifecycle.Error) {
				return false
			}
			errText := strings.TrimSpace(lifecycle.Error)
			if errText == "" {
				errText = "turn failed"
			}
			b.emitTurnErrorOnce(turnID, base, fmt.Errorf("%s", errText))
			return true
		}
		b.emitTurnTerminal(turnID, base)
		return true
	case cantofw.RunLifecycleTool:
		if lifecycle.Tool == nil {
			if hasSessionEvent {
				return b.translateEvent(ctx, ev, turnID)
			}
			return false
		}
		tool := lifecycle.Tool
		switch lifecycle.Status {
		case cantofw.RunLifecycleStarted:
			b.events <- ionsession.ToolCallStarted{
				Base:      base,
				ToolUseID: tool.ID,
				ToolName:  tool.Name,
				Args:      tool.Arguments,
			}
			b.events <- ionsession.StatusChanged{
				Base:   base,
				Status: fmt.Sprintf("Running %s...", tool.Name),
			}
		case cantofw.RunLifecycleUpdated:
			if tool.Delta != "" {
				b.events <- ionsession.ToolOutputDelta{
					Base:      base,
					ToolUseID: tool.ID,
					Delta:     tool.Delta,
					Snapshot:  tool.Snapshot,
				}
			}
		case cantofw.RunLifecycleCompleted,
			cantofw.RunLifecycleFailed,
			cantofw.RunLifecycleCanceled:
			var execErr error
			if tool.Error != "" {
				execErr = fmt.Errorf("%s", tool.Error)
			}
			b.events <- ionsession.ToolResult{
				Base:      base,
				ToolUseID: tool.ID,
				ToolName:  tool.Name,
				Result:    tool.Output,
				Error:     execErr,
			}
			status := activeToolsStatus(lifecycle.ActiveTools)
			if status == "" {
				status = "Thinking..."
			}
			b.events <- ionsession.StatusChanged{Base: base, Status: status}
		}
		return false
	case cantofw.RunLifecycleChild:
		b.translateRunChildLifecycle(base, lifecycle)
		return false
	case cantofw.RunLifecycleWait:
		b.translateRunWaitLifecycle(base, lifecycle)
		return false
	case cantofw.RunLifecycleApproval:
		b.translateRunApprovalLifecycle(base, lifecycle)
		return false
	case cantofw.RunLifecycleCompaction:
		switch lifecycle.Status {
		case cantofw.RunLifecycleStarted:
			b.events <- ionsession.StatusChanged{Base: base, Status: "Compacting context..."}
		case cantofw.RunLifecycleCompleted:
			b.events <- ionsession.StatusChanged{Base: base, Status: "Context compacted."}
		}
		return false
	case cantofw.RunLifecycleRetry:
		b.events <- ionsession.StatusChanged{Base: base, Status: retryLifecycleStatus(lifecycle.Retry)}
		return false
	}

	if hasSessionEvent {
		return b.translateEvent(ctx, ev, turnID)
	}
	return false
}

func (b *Backend) translateRunChildLifecycle(
	base ionsession.Base,
	lifecycle *cantofw.RunLifecycle,
) {
	if lifecycle == nil || lifecycle.Child == nil {
		return
	}
	child := lifecycle.Child
	switch lifecycle.Status {
	case cantofw.RunLifecycleRequested:
		b.events <- ionsession.ChildRequested{
			Base:      base,
			AgentName: child.ID,
			Query:     child.Task,
		}
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: fmt.Sprintf("Requesting child agent %s...", child.ID),
		}
	case cantofw.RunLifecycleStarted:
		b.events <- ionsession.ChildStarted{
			Base:      base,
			AgentName: child.ID,
			SessionID: child.SessionID,
		}
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: fmt.Sprintf("Child agent %s started (%s)", child.ID, child.SessionID),
		}
	case cantofw.RunLifecycleUpdated:
		message := strings.TrimSpace(child.Message)
		status := message
		if status == "" {
			status = strings.TrimSpace(child.Status)
		}
		if message != "" {
			b.events <- ionsession.ChildDelta{
				Base:      base,
				AgentName: child.ID,
				Delta:     child.Message,
			}
		}
		if status != "" {
			b.events <- ionsession.StatusChanged{
				Base:   base,
				Status: fmt.Sprintf("Child agent %s: %s", child.ID, status),
			}
		}
	case cantofw.RunLifecycleCompleted:
		b.events <- ionsession.ChildCompleted{
			Base:      base,
			AgentName: child.ID,
			Result:    child.Summary,
		}
		b.emitRunUsage(base, lifecycle.Usage)
	case cantofw.RunLifecycleBlocked:
		b.events <- ionsession.ChildBlocked{
			Base:      base,
			AgentName: child.ID,
			Reason:    child.Reason,
		}
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: fmt.Sprintf("Child agent %s blocked", child.ID),
		}
	case cantofw.RunLifecycleFailed:
		b.events <- ionsession.ChildFailed{
			Base:      base,
			AgentName: child.ID,
			Error:     child.Error,
		}
	case cantofw.RunLifecycleCanceled:
		b.events <- ionsession.ChildCanceled{
			Base:      base,
			AgentName: child.ID,
			Reason:    child.Reason,
		}
	}
}

func (b *Backend) translateRunApprovalLifecycle(
	base ionsession.Base,
	lifecycle *cantofw.RunLifecycle,
) {
	if lifecycle == nil || lifecycle.Approval == nil {
		return
	}
	approval := lifecycle.Approval
	switch lifecycle.Status {
	case cantofw.RunLifecycleRequested:
		target := strings.TrimSpace(approval.Tool)
		if target == "" {
			target = strings.TrimSpace(approval.Operation)
		}
		if target == "" {
			target = "tool call"
		}
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: fmt.Sprintf("Approval requested for %s...", target),
		}
	case cantofw.RunLifecycleCompleted:
		decision := strings.TrimSpace(string(approval.Decision))
		if decision == "" {
			decision = "resolved"
		}
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: fmt.Sprintf("Approval %s.", decision),
		}
	case cantofw.RunLifecycleCanceled:
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: "Approval canceled.",
		}
	}
}

func (b *Backend) translateRunWaitLifecycle(
	base ionsession.Base,
	lifecycle *cantofw.RunLifecycle,
) {
	if lifecycle == nil || lifecycle.Wait == nil {
		return
	}
	switch lifecycle.Status {
	case cantofw.RunLifecycleStarted:
		reason := strings.TrimSpace(lifecycle.Wait.Reason)
		if reason == "" {
			reason = "external input"
		}
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: fmt.Sprintf("Waiting for %s...", reason),
		}
	case cantofw.RunLifecycleCompleted:
		b.events <- ionsession.StatusChanged{
			Base:   base,
			Status: "Wait resolved.",
		}
	}
}

func activeToolsStatus(tools []cantofw.RunToolLifecycle) string {
	active := 0
	name := ""
	for _, tool := range tools {
		if tool.ID == "" {
			continue
		}
		active++
		if name == "" {
			name = strings.TrimSpace(tool.Name)
		}
	}
	switch {
	case active == 0:
		return ""
	case active == 1 && name != "":
		return fmt.Sprintf("Running %s...", name)
	case active == 1:
		return "Running tool..."
	default:
		return fmt.Sprintf("Running %d tools...", active)
	}
}

func retryLifecycleStatus(retry *cantofw.RunRetryLifecycle) string {
	if retry == nil {
		return "Retrying..."
	}
	if retry.Scope == "overflow_recovery" {
		return "Recovering from context overflow..."
	}
	if retry.Scope != "provider" {
		return "Retrying..."
	}
	var err error
	if strings.TrimSpace(retry.Error) != "" {
		err = fmt.Errorf("%s", retry.Error)
	}
	return retryStatus(llm.RetryEvent{
		Attempt: retry.Attempt,
		Delay:   time.Duration(retry.DelayMillis) * time.Millisecond,
		Err:     err,
	})
}
