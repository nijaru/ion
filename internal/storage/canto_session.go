package storage

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
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
		CreatedAt: metadataTime(s.meta.CreatedAt),
	}
}

func (s *cantoSession) Append(ctx context.Context, event Event) error {
	var title string
	var preview string
	var err error
	switch e := event.(type) {
	case Subagent:
		preview = sessionSummary(e.Content)
		ev := newStoredEvent(s.id, ionSubagentEvent, e, e.TS)
		err = s.store.canto.Save(ctx, ev)
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
	default:
		return fmt.Errorf("canto storage cannot append unsupported %T events", event)
	}

	if err != nil {
		return err
	}

	return s.store.UpdateSession(
		ctx,
		SessionInfo{ID: s.id, Title: title, Summary: preview, LastPreview: preview},
	)
}

func (s *cantoSession) AppendModelMessage(ctx context.Context, message llm.Message) error {
	if isEmptyModelMessage(message) {
		return nil
	}
	if err := s.store.canto.Save(ctx, session.NewMessage(s.id, message)); err != nil {
		return err
	}
	text := message.TextContent()
	title := ""
	if message.Role == llm.RoleUser {
		title = sessionTitle(text)
	}
	preview := sessionSummary(text)
	return s.store.UpdateSession(
		ctx,
		SessionInfo{ID: s.id, Title: title, LastPreview: preview},
	)
}

func (s *cantoSession) ModelMessages(ctx context.Context) ([]llm.Message, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return nil, err
	}
	return sess.EffectiveMessages()
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

func isEmptyModelMessage(message llm.Message) bool {
	return strings.TrimSpace(message.TextContent()) == "" &&
		strings.TrimSpace(message.Reasoning) == "" &&
		len(message.ThinkingBlocks) == 0 &&
		len(message.Calls) == 0 &&
		len(message.Parts) == 0
}

func (s *cantoSession) LastStatus(ctx context.Context) (string, error) {
	projection, err := s.progressProjection(ctx)
	if err != nil {
		return "", err
	}
	return projection.lastStatus, nil
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
		session.EventType("routing_decision"):
		return true
	default:
		return false
	}
}

func (s *cantoSession) Usage(ctx context.Context) (int, int, float64, error) {
	projection, err := s.progressProjection(ctx)
	if err != nil {
		return 0, 0, 0, err
	}
	total := projection.totals()
	return total.input, total.output, total.cost, nil
}

func (s *cantoSession) Close() error {
	return nil
}

type usageAccumulator struct {
	input  int
	output int
	cost   float64
}

func (a *usageAccumulator) add(other usageAccumulator) {
	a.input += other.input
	a.output += other.output
	a.cost += other.cost
}

func (a *usageAccumulator) addValues(input, output int, cost float64) {
	a.input += input
	a.output += output
	a.cost += cost
}

func (a *usageAccumulator) addUsage(usage llm.Usage) {
	a.addValues(usage.InputTokens, usage.OutputTokens, usage.Cost)
}

func usageHasValue(usage llm.Usage) bool {
	return usage.InputTokens != 0 || usage.OutputTokens != 0 || usage.Cost != 0
}
