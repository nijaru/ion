package storage

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/canto/session"
)

type cantoSession struct {
	id    string
	store *cantoStore
	meta  Meta
}

func (s *cantoSession) ID() string { return s.id }

func (s *cantoSession) Meta() Metadata {
	return Metadata{
		ID:        s.meta.ID,
		CWD:       s.meta.CWD,
		Model:     s.meta.Model,
		Branch:    s.meta.Branch,
		CreatedAt: time.Unix(s.meta.CreatedAt, 0),
	}
}

func (s *cantoSession) Append(ctx context.Context, event any) error {
	var title string
	var preview string
	var err error
	switch e := event.(type) {
	case User:
		return modelVisibleAppendError(event)
	case Agent:
		content, reasoning := agentMessagePayload(e)
		if !hasAgentMessagePayload(content, reasoning) {
			return nil
		}
		return modelVisibleAppendError(event)
	case Subagent:
		preview = sessionSummary(e.Content)
		ev := newStoredEvent(s.id, ionSubagentEvent, e, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case ToolUse:
		return modelVisibleAppendError(event)
	case ToolResult:
		return modelVisibleAppendError(event)
	case Status:
		if !isDurableResumeStatus(e.Status) {
			return nil
		}
		ev := newStoredEvent(s.id, session.EventType("status_changed"), map[string]any{
			"status": e.Status,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case System:
		preview = ""
		ev := newStoredEvent(s.id, ionSystemEvent, e, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case TokenUsage:
		ev := newStoredEvent(s.id, session.EventType("token_usage"), map[string]any{
			"input":  e.Input,
			"output": e.Output,
			"cost":   e.Cost,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case RoutingDecision:
		ev := newStoredEvent(s.id, session.EventType("routing_decision"), map[string]any{
			"decision":         e.Decision,
			"reason":           e.Reason,
			"model_slot":       e.ModelSlot,
			"provider":         e.Provider,
			"model":            e.Model,
			"reasoning":        e.Reasoning,
			"max_session_cost": e.MaxSessionCost,
			"max_turn_cost":    e.MaxTurnCost,
			"session_cost":     e.SessionCost,
			"turn_cost":        e.TurnCost,
			"stop_reason":      e.StopReason,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case EscalationNotification:
		ev := newStoredEvent(s.id, session.EventType("escalation_notification"), map[string]any{
			"request_id": e.RequestID,
			"channel":    e.Channel,
			"target":     e.Target,
			"status":     e.Status,
			"detail":     e.Detail,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	default:
		return nil
	}

	if err != nil {
		return err
	}

	return s.store.UpdateSession(
		ctx,
		SessionInfo{ID: s.id, Title: title, Summary: preview, LastPreview: preview},
	)
}

func newStoredEvent(
	sessionID string,
	eventType session.EventType,
	data any,
	unixTS int64,
) session.Event {
	ev := session.NewEvent(sessionID, eventType, data)
	if unixTS > 0 {
		ev.Timestamp = time.Unix(unixTS, 0).UTC()
	}
	return ev
}

func modelVisibleAppendError(event any) error {
	return fmt.Errorf(
		"canto storage cannot append model-visible %T events; use the Canto runner",
		event,
	)
}

func (s *cantoSession) LastStatus(ctx context.Context) (string, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return "", err
	}

	var lastStatus string
	for _, ev := range sess.Events() {
		if ev.Type == session.EventType("status_changed") {
			var data struct {
				Status string `json:"status"`
			}
			if err := ev.UnmarshalData(&data); err != nil {
				continue
			}
			if isDurableResumeStatus(data.Status) {
				lastStatus = strings.TrimSpace(data.Status)
			} else {
				lastStatus = ""
			}
			continue
		}
		if lastStatus != "" && clearsDurableResumeStatus(ev.Type) {
			lastStatus = ""
		}
	}
	return lastStatus, nil
}

func isDurableResumeStatus(status string) bool {
	status = strings.TrimSpace(status)
	if status == "" {
		return false
	}
	return strings.Contains(strings.ToLower(status), "retrying")
}

func clearsDurableResumeStatus(eventType session.EventType) bool {
	switch eventType {
	case session.MessageAdded,
		session.ContextAdded,
		session.TurnCompleted,
		session.ToolCompleted,
		session.ApprovalResolved,
		session.ApprovalCanceled,
		session.CompactionTriggered,
		ionSystemEvent,
		ionSubagentEvent,
		session.EventType("token_usage"),
		session.EventType("routing_decision"),
		session.EventType("escalation_notification"):
		return true
	default:
		return false
	}
}

func (s *cantoSession) Usage(ctx context.Context) (int, int, float64, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return 0, 0, 0, err
	}

	var input, output int
	var cost float64

	for _, ev := range sess.Events() {
		// Use literal string for TokenUsage event type
		if ev.Type == "token_usage" {
			var data struct {
				Input  int     `json:"input"`
				Output int     `json:"output"`
				Cost   float64 `json:"cost"`
			}
			if err := ev.UnmarshalData(&data); err == nil {
				input += data.Input
				output += data.Output
				cost += data.Cost
			}
		}
	}

	return input, output, cost, nil
}

func (s *cantoSession) Close() error {
	return nil
}
