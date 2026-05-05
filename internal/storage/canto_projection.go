package storage

import (
	"context"
	"strings"
	"time"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/tooldisplay"
)

func (s *cantoSession) Entries(ctx context.Context) ([]ionsession.Entry, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return nil, err
	}

	history, err := sess.EffectiveEntries()
	if err != nil {
		return nil, err
	}

	entries := make([]ionsession.Entry, 0, len(history))
	effectiveByEventID := make(map[string]session.HistoryEntry, len(history))
	for _, entry := range history {
		if entry.EventID == "" {
			if display, ok := displayHistoryEntry(s.meta.CWD, entry); ok {
				entries = append(entries, display)
			}
			continue
		}
		effectiveByEventID[entry.EventID] = entry
	}

	seenEffective := make(map[string]bool, len(effectiveByEventID))
	for _, ev := range sess.Events() {
		if entry, ok := effectiveByEventID[ev.ID.String()]; ok {
			if display, ok := displayHistoryEntry(s.meta.CWD, entry); ok {
				display = withEntryTimestamp(display, ev.Timestamp)
				entries = append(entries, display)
			}
			seenEffective[entry.EventID] = true
			continue
		}
		if display, ok := displayEventEntry(ev); ok {
			display = withEntryTimestamp(display, ev.Timestamp)
			entries = append(entries, display)
		}
	}
	for _, entry := range history {
		if entry.EventID == "" || seenEffective[entry.EventID] {
			continue
		}
		if display, ok := displayHistoryEntry(s.meta.CWD, entry); ok {
			entries = append(entries, display)
		}
	}
	return normalizeDisplayEntries(entries), nil
}

func withEntryTimestamp(entry ionsession.Entry, timestamp time.Time) ionsession.Entry {
	if !timestamp.IsZero() {
		entry.Timestamp = timestamp.UTC()
	}
	return entry
}

func displayHistoryEntry(workdir string, entry session.HistoryEntry) (ionsession.Entry, bool) {
	if display, ok := displayContextEntry(entry); ok {
		return display, true
	}
	msg := entry.Message
	switch msg.Role {
	case llm.RoleUser:
		return ionsession.Entry{
			Role:    ionsession.User,
			Content: msg.Content,
		}, true
	case llm.RoleAssistant:
		return ionsession.Entry{
			Role:      ionsession.Agent,
			Content:   msg.Content,
			Reasoning: msg.Reasoning,
		}, true
	case llm.RoleTool:
		name := msg.Name
		args := ""
		isError := false
		if entry.Tool != nil {
			if entry.Tool.Name != "" {
				name = entry.Tool.Name
			}
			args = entry.Tool.Arguments
			isError = entry.Tool.IsError || strings.TrimSpace(entry.Tool.Error) != ""
		}
		title := tooldisplay.Title(name, args, tooldisplay.Options{Workdir: workdir})
		if title == "" {
			title = "tool"
		}
		return ionsession.Entry{
			Role:    ionsession.Tool,
			Title:   title,
			Content: msg.Content,
			IsError: isError,
		}, true
	case llm.RoleSystem, llm.RoleDeveloper:
		return ionsession.Entry{
			Role:    ionsession.System,
			Content: msg.Content,
		}, true
	default:
		return ionsession.Entry{}, false
	}
}

func normalizeDisplayEntries(entries []ionsession.Entry) []ionsession.Entry {
	normalized := make([]ionsession.Entry, 0, len(entries))
	for _, entry := range entries {
		if entry.Role == ionsession.Agent {
			if strings.TrimSpace(entry.Content) == "" && strings.TrimSpace(entry.Reasoning) == "" {
				continue
			}
		}
		normalized = append(normalized, entry)
	}
	return normalized
}

func displayContextEntry(entry session.HistoryEntry) (ionsession.Entry, bool) {
	if entry.EventType != session.ContextAdded {
		return ionsession.Entry{}, false
	}
	switch entry.ContextKind {
	case session.ContextKindSummary, session.ContextKindWorkingSet, session.ContextKindBootstrap:
		return ionsession.Entry{
			Role:    ionsession.System,
			Content: entry.Message.Content,
		}, true
	default:
		return ionsession.Entry{}, false
	}
}

func displayEventEntry(ev session.Event) (ionsession.Entry, bool) {
	switch ev.Type {
	case ionSystemEvent:
		var data System
		if err := ev.UnmarshalData(&data); err != nil {
			return ionsession.Entry{}, false
		}
		return ionsession.Entry{
			Role:    ionsession.System,
			Content: data.Content,
		}, true
	case ionSubagentEvent:
		var data Subagent
		if err := ev.UnmarshalData(&data); err != nil {
			return ionsession.Entry{}, false
		}
		return ionsession.Entry{
			Role:    ionsession.Subagent,
			Title:   data.Name,
			Content: data.Content,
			IsError: data.IsError,
		}, true
	default:
		return ionsession.Entry{}, false
	}
}
