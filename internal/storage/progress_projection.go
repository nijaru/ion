package storage

import (
	"context"
	"database/sql"
	"errors"
	"strings"
	"time"

	csession "github.com/nijaru/ion/session"
)

type progressProjection struct {
	sessionID    string
	cutoffSeq    int64
	cutoffEvent  string
	lastStatus   string
	usage        usageAccumulator
	pendingUsage usageAccumulator
}

func (s *cantoSession) progressProjection(ctx context.Context) (progressProjection, error) {
	projection, ok, err := s.store.loadProgressProjection(ctx, s.id)
	if err != nil {
		return progressProjection{}, err
	}
	if !ok {
		projection, err = s.buildProgressProjection(ctx)
		if err != nil {
			return progressProjection{}, err
		}
		if err := s.store.saveProgressProjection(ctx, projection); err != nil {
			return progressProjection{}, err
		}
		return projection, nil
	}

	events, err := s.store.canto.EventsAfter(ctx, s.id, projection.cutoffSeq)
	if err != nil {
		return progressProjection{}, err
	}
	if len(events) == 0 {
		return projection, nil
	}
	projection.applyEvents(events)
	if err := s.store.saveProgressProjection(ctx, projection); err != nil {
		return progressProjection{}, err
	}
	return projection, nil
}

func (s *cantoSession) buildProgressProjection(ctx context.Context) (progressProjection, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return progressProjection{}, err
	}
	projection := progressProjection{sessionID: s.id}
	projection.applyEvents(sess.Events())
	return projection, nil
}

func (p *progressProjection) applyEvents(events []csession.Event) {
	for _, ev := range events {
		if ev.Seq > p.cutoffSeq {
			p.cutoffSeq = ev.Seq
		}
		p.cutoffEvent = ev.ID.String()
		p.applyStatusEvent(ev)
		p.applyUsageEvent(ev)
	}
}

func (p *progressProjection) applyStatusEvent(ev csession.Event) {
	if ev.Type == csession.EventType("status_changed") {
		var data struct {
			Status string `json:"status"`
		}
		if err := ev.UnmarshalData(&data); err != nil {
			return
		}
		if isDurableResumeStatus(data.Status) {
			p.lastStatus = strings.TrimSpace(data.Status)
		} else {
			p.lastStatus = ""
		}
		return
	}
	if p.lastStatus != "" && clearsDurableResumeStatus(ev.Type) {
		p.lastStatus = ""
	}
}

func (p *progressProjection) applyUsageEvent(ev csession.Event) {
	switch ev.Type {
	case csession.TurnStarted:
		p.usage.add(p.pendingUsage)
		p.pendingUsage = usageAccumulator{}
	case csession.EventType("token_usage"):
		var data struct {
			Input  int     `json:"input"`
			Output int     `json:"output"`
			Cost   float64 `json:"cost"`
		}
		if err := ev.UnmarshalData(&data); err == nil {
			p.pendingUsage.addValues(data.Input, data.Output, data.Cost)
		}
	case csession.TurnCompleted:
		data, ok, err := ev.TurnCompletedData()
		if err == nil && ok && usageHasValue(data.Usage) {
			p.usage.addUsage(data.Usage)
		} else {
			p.usage.add(p.pendingUsage)
		}
		p.pendingUsage = usageAccumulator{}
	}
}

func (p progressProjection) totals() usageAccumulator {
	total := p.usage
	total.add(p.pendingUsage)
	return total
}

func (s *cantoStore) loadProgressProjection(
	ctx context.Context,
	sessionID string,
) (progressProjection, bool, error) {
	var (
		projection         progressProjection
		rawEntries         []byte
		rawToolCalls       []byte
		rawToolStarts      []byte
		rawToolCompletions []byte
		lastStatus         sql.NullString
		cutoffID           sql.NullString
	)
	err := s.db.QueryRowContext(
		ctx,
		`SELECT cutoff_seq, cutoff_event_id, entries_json, last_status,
		        usage_input, usage_output, usage_cost,
		        pending_input, pending_output, pending_cost,
		        tool_calls_json, tool_starts_json, tool_completions_json
		   FROM session_display_projection WHERE session_id = ?`,
		sessionID,
	).Scan(
		&projection.cutoffSeq,
		&cutoffID,
		&rawEntries,
		&lastStatus,
		&projection.usage.input,
		&projection.usage.output,
		&projection.usage.cost,
		&projection.pendingUsage.input,
		&projection.pendingUsage.output,
		&projection.pendingUsage.cost,
		&rawToolCalls,
		&rawToolStarts,
		&rawToolCompletions,
	)
	if errors.Is(err, sql.ErrNoRows) {
		return progressProjection{}, false, nil
	}
	if err != nil {
		return progressProjection{}, false, err
	}
	projection.sessionID = sessionID
	projection.cutoffEvent = cutoffID.String
	projection.lastStatus = lastStatus.String
	return projection, true, nil
}

func (s *cantoStore) saveProgressProjection(
	ctx context.Context,
	projection progressProjection,
) error {
	_, err := s.db.ExecContext(
		ctx,
		`INSERT INTO session_display_projection (
			session_id, cutoff_seq, cutoff_event_id, entries_json, last_status,
			usage_input, usage_output, usage_cost,
			pending_input, pending_output, pending_cost,
			tool_calls_json, tool_starts_json, tool_completions_json, updated_at
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(session_id) DO UPDATE SET
			cutoff_seq = excluded.cutoff_seq,
			cutoff_event_id = excluded.cutoff_event_id,
			entries_json = excluded.entries_json,
			last_status = excluded.last_status,
			usage_input = excluded.usage_input,
			usage_output = excluded.usage_output,
			usage_cost = excluded.usage_cost,
			pending_input = excluded.pending_input,
			pending_output = excluded.pending_output,
			pending_cost = excluded.pending_cost,
			tool_calls_json = excluded.tool_calls_json,
			tool_starts_json = excluded.tool_starts_json,
			tool_completions_json = excluded.tool_completions_json,
			updated_at = excluded.updated_at
		WHERE excluded.cutoff_seq >= session_display_projection.cutoff_seq`,
		projection.sessionID,
		projection.cutoffSeq,
		projection.cutoffEvent,
		[]byte("[]"),
		projection.lastStatus,
		projection.usage.input,
		projection.usage.output,
		projection.usage.cost,
		projection.pendingUsage.input,
		projection.pendingUsage.output,
		projection.pendingUsage.cost,
		[]byte("{}"),
		[]byte("{}"),
		[]byte("[]"),
		metadataTimestamp(time.Now()),
	)
	return err
}
