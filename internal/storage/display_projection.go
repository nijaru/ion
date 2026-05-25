package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"time"

	jsonv2 "github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/transcript"
)

type displayProjection struct {
	sessionID       string
	cutoffSeq       int64
	cutoffEvent     string
	entries         []ionsession.Entry
	lastStatus      string
	usage           usageAccumulator
	pendingUsage    usageAccumulator
	toolCalls       map[string]projectedToolCall
	toolStarts      map[string]csession.ToolStartedData
	toolCompletions []projectedToolCompletion
}

type projectedToolCall struct {
	ID        string `json:"id"`
	Name      string `json:"name,omitzero"`
	Arguments string `json:"args,omitzero"`
}

type projectedToolCompletion struct {
	EventID   string                     `json:"event_id"`
	ID        string                     `json:"id"`
	Data      csession.ToolCompletedData `json:"data"`
	Timestamp time.Time                  `json:"timestamp,omitzero"`
	Displayed bool                       `json:"displayed,omitzero"`
}

func (s *cantoSession) displayProjection(ctx context.Context) (displayProjection, error) {
	projection, ok, err := s.store.loadDisplayProjection(ctx, s.id)
	if err != nil {
		return displayProjection{}, err
	}
	if !ok {
		projection, err = s.buildDisplayProjection(ctx)
		if err != nil {
			return displayProjection{}, err
		}
		if err := s.store.saveDisplayProjection(ctx, projection); err != nil {
			return displayProjection{}, err
		}
		return projection, nil
	}

	events, err := s.store.canto.EventsAfter(ctx, s.id, projection.cutoffSeq)
	if err != nil {
		return displayProjection{}, err
	}
	if len(events) == 0 {
		return projection, nil
	}
	projection.applyEvents(s.meta.CWD, events)
	if err := s.store.saveDisplayProjection(ctx, projection); err != nil {
		return displayProjection{}, err
	}
	return projection, nil
}

func (s *cantoSession) buildDisplayProjection(ctx context.Context) (displayProjection, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return displayProjection{}, err
	}
	events := sess.Events()
	entries, err := displayEntriesFromSession(s.meta.CWD, sess)
	if err != nil {
		return displayProjection{}, err
	}
	projection := displayProjection{
		sessionID: s.id,
		entries:   entries,
	}
	projection.ensureToolState()
	projection.applyNonDisplayEvents(events)
	return projection, nil
}

func (p *displayProjection) ensureToolState() {
	if p.toolCalls == nil {
		p.toolCalls = make(map[string]projectedToolCall)
	}
	if p.toolStarts == nil {
		p.toolStarts = make(map[string]csession.ToolStartedData)
	}
}

func (p *displayProjection) clearToolState() {
	clear(p.toolCalls)
	clear(p.toolStarts)
	p.toolCompletions = nil
}

func (p *displayProjection) applyNonDisplayEvents(events []csession.Event) {
	for _, ev := range events {
		if ev.Seq > p.cutoffSeq {
			p.cutoffSeq = ev.Seq
		}
		p.cutoffEvent = ev.ID.String()
		p.trackToolStateOnly(ev)
		p.applyStatusEvent(ev)
		p.applyUsageEvent(ev)
	}
}

func (p *displayProjection) applyEvents(workdir string, events []csession.Event) {
	for _, ev := range events {
		if ev.Seq > p.cutoffSeq {
			p.cutoffSeq = ev.Seq
		}
		p.cutoffEvent = ev.ID.String()
		p.applyEntryEvent(workdir, ev)
		p.applyStatusEvent(ev)
		p.applyUsageEvent(ev)
	}
	p.entries = transcript.Normalize(p.entries)
}

func (p *displayProjection) applyEntryEvent(workdir string, ev csession.Event) {
	if snapshot, ok, err := ev.ProjectionSnapshot(); err == nil && ok &&
		usableDisplaySnapshot(snapshot) {
		p.entries = displaySnapshotEntries(workdir, snapshot)
		p.clearToolState()
		return
	}
	if snapshot, ok, err := ev.CompactionSnapshot(); err == nil && ok &&
		usableDisplaySnapshot(snapshot) {
		p.entries = displaySnapshotEntries(workdir, snapshot)
		p.clearToolState()
		return
	}
	if display, ok := displayEventEntry(ev); ok {
		p.entries = append(p.entries, transcript.WithTimestamp(display, ev.Timestamp))
		return
	}
	switch ev.Type {
	case csession.MessageAdded:
		p.applyMessageEntry(workdir, ev)
	case csession.ContextAdded:
		p.flushCompletedTools(workdir)
		p.clearToolState()
		p.applyContextEntry(workdir, ev)
	case csession.ToolStarted:
		p.applyToolStarted(ev)
	case csession.ToolCompleted:
		p.applyToolCompleted(ev, false)
	case csession.TurnCompleted:
		p.flushCompletedTools(workdir)
		p.clearToolState()
	}
}

func (p *displayProjection) applyMessageEntry(workdir string, ev csession.Event) {
	var msg llm.Message
	if err := ev.UnmarshalData(&msg); err != nil {
		return
	}
	switch msg.Role {
	case llm.RoleAssistant:
		p.flushCompletedTools(workdir)
		p.clearToolState()
		p.setToolCalls(msg.Calls)
	case llm.RoleTool:
		p.applyToolMessage(workdir, ev, msg)
		return
	default:
		p.flushCompletedTools(workdir)
		p.clearToolState()
	}
	entry, ok := transcript.New(workdir).HistoryEntry(csession.HistoryEntry{
		EventID:   ev.ID.String(),
		EventType: ev.Type,
		Message:   msg,
	})
	if !ok {
		return
	}
	p.entries = append(p.entries, transcript.WithTimestamp(entry, ev.Timestamp))
}

func (p *displayProjection) applyContextEntry(workdir string, ev csession.Event) {
	var contextEntry csession.ContextEntry
	if err := ev.UnmarshalData(&contextEntry); err != nil {
		return
	}
	if contextEntry.Kind == "" {
		contextEntry.Kind = csession.ContextKindGeneric
	}
	entry, ok := transcript.New(workdir).HistoryEntry(csession.HistoryEntry{
		EventID:     ev.ID.String(),
		EventType:   ev.Type,
		ContextKind: contextEntry.Kind,
		Message:     llm.Message{Role: llm.RoleUser, Content: contextEntry.Content},
	})
	if !ok {
		return
	}
	p.entries = append(p.entries, transcript.WithTimestamp(entry, ev.Timestamp))
}

func (p *displayProjection) applyToolMessage(
	workdir string,
	ev csession.Event,
	msg llm.Message,
) {
	if completion, ok := p.toolCompletion(msg.ToolID); ok && completion.Displayed {
		p.removeTool(msg.ToolID)
		return
	}
	tool := p.toolHistoryForMessage(msg)
	entry, ok := transcript.New(workdir).HistoryEntry(csession.HistoryEntry{
		EventID:   ev.ID.String(),
		EventType: ev.Type,
		Message:   msg,
		Tool:      &tool,
	})
	if !ok {
		return
	}
	p.entries = append(p.entries, transcript.WithTimestamp(entry, ev.Timestamp))
	p.removeTool(msg.ToolID)
}

func (p *displayProjection) setToolCalls(calls []llm.Call) {
	p.ensureToolState()
	for _, call := range calls {
		if call.ID == "" {
			continue
		}
		p.toolCalls[call.ID] = projectedToolCall{
			ID:        call.ID,
			Name:      call.Function.Name,
			Arguments: call.Function.Arguments,
		}
	}
}

func (p *displayProjection) applyToolStarted(ev csession.Event) {
	data, ok, err := ev.ToolStartedData()
	if err != nil || !ok || data.ID == "" {
		return
	}
	p.ensureToolState()
	p.toolStarts[data.ID] = data
}

func (p *displayProjection) applyToolCompleted(ev csession.Event, displayed bool) {
	data, ok, err := ev.ToolCompletedData()
	if err != nil || !ok || data.ID == "" {
		return
	}
	p.ensureToolState()
	p.removeToolCompletion(data.ID)
	p.toolCompletions = append(p.toolCompletions, projectedToolCompletion{
		EventID:   ev.ID.String(),
		ID:        data.ID,
		Data:      data,
		Timestamp: ev.Timestamp,
		Displayed: displayed,
	})
}

func (p *displayProjection) flushCompletedTools(workdir string) {
	if len(p.toolCompletions) == 0 {
		return
	}
	for _, completion := range p.toolCompletions {
		if _, ok := p.toolCalls[completion.ID]; !ok {
			continue
		}
		entry, ok := p.completedToolEntry(workdir, completion)
		if !ok {
			continue
		}
		p.entries = append(p.entries, entry)
	}
	p.toolCompletions = nil
}

func (p *displayProjection) completedToolEntry(
	workdir string,
	completion projectedToolCompletion,
) (ionsession.Entry, bool) {
	name := completion.Data.Tool
	if start, ok := p.toolStarts[completion.ID]; ok && name == "" {
		name = start.Tool
	}
	if call, ok := p.toolCalls[completion.ID]; ok && name == "" {
		name = call.Name
	}
	content := completion.Data.Output
	if completion.Data.Error != "" && !strings.Contains(content, completion.Data.Error) {
		content = strings.TrimSpace(
			strings.TrimSpace(content) + "\nError: " + completion.Data.Error,
		)
	}
	msg := llm.Message{
		Role:    llm.RoleTool,
		ToolID:  completion.ID,
		Name:    name,
		Content: content,
	}
	tool := p.toolHistoryForMessage(msg)
	entry, ok := transcript.New(workdir).HistoryEntry(csession.HistoryEntry{
		EventID:   completion.EventID,
		EventType: csession.ToolCompleted,
		Message:   msg,
		Tool:      &tool,
	})
	if !ok {
		return ionsession.Entry{}, false
	}
	return transcript.WithTimestamp(entry, completion.Timestamp), true
}

func (p *displayProjection) toolHistoryForMessage(msg llm.Message) csession.ToolHistory {
	id := msg.ToolID
	tool := csession.ToolHistory{
		ID:   id,
		Name: msg.Name,
	}
	if start, ok := p.toolStarts[id]; ok {
		if tool.Name == "" {
			tool.Name = start.Tool
		}
		if tool.Arguments == "" {
			tool.Arguments = start.Arguments
		}
		if tool.IdempotencyKey == "" {
			tool.IdempotencyKey = start.IdempotencyKey
		}
	}
	if call, ok := p.toolCalls[id]; ok {
		if tool.Name == "" {
			tool.Name = call.Name
		}
		if tool.Arguments == "" {
			tool.Arguments = call.Arguments
		}
	}
	if completion, ok := p.toolCompletion(id); ok {
		if tool.Name == "" {
			tool.Name = completion.Data.Tool
		}
		if tool.IdempotencyKey == "" {
			tool.IdempotencyKey = completion.Data.IdempotencyKey
		}
		if completion.Data.Error != "" {
			tool.IsError = true
			tool.Error = completion.Data.Error
		}
	}
	return tool
}

func (p *displayProjection) toolCompletion(id string) (projectedToolCompletion, bool) {
	for _, completion := range p.toolCompletions {
		if completion.ID == id {
			return completion, true
		}
	}
	return projectedToolCompletion{}, false
}

func (p *displayProjection) removeTool(id string) {
	if id == "" {
		return
	}
	delete(p.toolCalls, id)
	delete(p.toolStarts, id)
	p.removeToolCompletion(id)
}

func (p *displayProjection) removeToolCompletion(id string) {
	if id == "" || len(p.toolCompletions) == 0 {
		return
	}
	filtered := p.toolCompletions[:0]
	for _, completion := range p.toolCompletions {
		if completion.ID != id {
			filtered = append(filtered, completion)
		}
	}
	p.toolCompletions = filtered
}

func (p *displayProjection) trackToolStateOnly(ev csession.Event) {
	switch ev.Type {
	case csession.MessageAdded:
		var msg llm.Message
		if err := ev.UnmarshalData(&msg); err != nil {
			return
		}
		switch msg.Role {
		case llm.RoleAssistant:
			p.clearToolState()
			p.setToolCalls(msg.Calls)
		case llm.RoleTool:
			p.removeTool(msg.ToolID)
		default:
			p.clearToolState()
		}
	case csession.ContextAdded:
		p.clearToolState()
	case csession.ToolStarted:
		p.applyToolStarted(ev)
	case csession.ToolCompleted:
		p.applyToolCompleted(ev, true)
	case csession.TurnCompleted:
		p.clearToolState()
	}
}

func (p *displayProjection) applyStatusEvent(ev csession.Event) {
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

func (p *displayProjection) applyUsageEvent(ev csession.Event) {
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

func (p displayProjection) totals() usageAccumulator {
	total := p.usage
	total.add(p.pendingUsage)
	return total
}

func displaySnapshotEntries(
	workdir string,
	snapshot csession.CompactionSnapshot,
) []ionsession.Entry {
	return transcript.New(workdir).SnapshotEntries(snapshot)
}

func (s *cantoStore) loadDisplayProjection(
	ctx context.Context,
	sessionID string,
) (displayProjection, bool, error) {
	var (
		projection         displayProjection
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
		return displayProjection{}, false, nil
	}
	if err != nil {
		return displayProjection{}, false, err
	}
	projection.sessionID = sessionID
	projection.cutoffEvent = cutoffID.String
	projection.lastStatus = lastStatus.String
	if err := jsonv2.Unmarshal(rawEntries, &projection.entries); err != nil {
		return displayProjection{}, false, fmt.Errorf("decode display projection: %w", err)
	}
	projection.ensureToolState()
	if err := decodeProjectionJSON(rawToolCalls, &projection.toolCalls); err != nil {
		return displayProjection{}, false, fmt.Errorf(
			"decode display projection tool calls: %w",
			err,
		)
	}
	if err := decodeProjectionJSON(rawToolStarts, &projection.toolStarts); err != nil {
		return displayProjection{}, false, fmt.Errorf(
			"decode display projection tool starts: %w",
			err,
		)
	}
	if err := decodeProjectionJSON(rawToolCompletions, &projection.toolCompletions); err != nil {
		return displayProjection{}, false, fmt.Errorf(
			"decode display projection tool completions: %w",
			err,
		)
	}
	projection.ensureToolState()
	return projection, true, nil
}

func (s *cantoStore) saveDisplayProjection(
	ctx context.Context,
	projection displayProjection,
) error {
	projection.ensureToolState()
	rawEntries, err := jsonv2.Marshal(projection.entries)
	if err != nil {
		return fmt.Errorf("encode display projection: %w", err)
	}
	rawToolCalls, err := jsonv2.Marshal(projection.toolCalls)
	if err != nil {
		return fmt.Errorf("encode display projection tool calls: %w", err)
	}
	rawToolStarts, err := jsonv2.Marshal(projection.toolStarts)
	if err != nil {
		return fmt.Errorf("encode display projection tool starts: %w", err)
	}
	rawToolCompletions, err := jsonv2.Marshal(projection.toolCompletions)
	if err != nil {
		return fmt.Errorf("encode display projection tool completions: %w", err)
	}
	_, err = s.db.ExecContext(
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
		rawEntries,
		projection.lastStatus,
		projection.usage.input,
		projection.usage.output,
		projection.usage.cost,
		projection.pendingUsage.input,
		projection.pendingUsage.output,
		projection.pendingUsage.cost,
		rawToolCalls,
		rawToolStarts,
		rawToolCompletions,
		metadataTimestamp(time.Now()),
	)
	return err
}

func decodeProjectionJSON[T any](raw []byte, target *T) error {
	if len(raw) == 0 {
		return nil
	}
	return jsonv2.Unmarshal(raw, target)
}
