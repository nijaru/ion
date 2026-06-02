package session

import (
	"strings"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/tool"
)

type Projector struct {
	workdir string
}

func NewProjector(workdir string) Projector {
	return Projector{workdir: workdir}
}

func WithTimestamp(entry Entry, timestamp time.Time) Entry {
	if !timestamp.IsZero() {
		entry.Timestamp = timestamp.UTC()
	}
	return entry
}

func SetTimestamp(entry *Entry, timestamp time.Time) {
	if entry != nil && !timestamp.IsZero() {
		entry.Timestamp = timestamp.UTC()
	}
}

func Normalize(entries []Entry) []Entry {
	normalized := make([]Entry, 0, len(entries))
	for _, entry := range entries {
		if emptyAgent(entry) {
			continue
		}
		normalized = append(normalized, entry)
	}
	return normalized
}

func EntryUser(content string, timestamp time.Time) (Entry, bool) {
	if strings.TrimSpace(content) == "" {
		return Entry{}, false
	}
	return WithTimestamp(Entry{
		Role:    RoleUser,
		Content: content,
	}, timestamp), true
}

func EntryAgent(content, reasoning string, timestamp time.Time) (Entry, bool) {
	entry := WithTimestamp(Entry{
		Role:      RoleAgent,
		Content:   content,
		Reasoning: reasoning,
	}, timestamp)
	if emptyAgent(entry) {
		return Entry{}, false
	}
	return entry, true
}

func EntrySystem(content string, timestamp time.Time) (Entry, bool) {
	return WithTimestamp(Entry{
		Role:    RoleSystem,
		Content: content,
	}, timestamp), true
}

func Tool(title, content string, isError bool, timestamp time.Time) (Entry, bool) {
	if title == "" {
		title = "tool"
	}
	return WithTimestamp(Entry{
		Role:    RoleTool,
		Title:   title,
		Content: content,
		IsError: isError,
	}, timestamp), true
}

func EntrySubagent(title, content string, isError bool, timestamp time.Time) (Entry, bool) {
	return WithTimestamp(Entry{
		Role:    RoleSubagent,
		Title:   title,
		Content: content,
		IsError: isError,
	}, timestamp), true
}

func (p Projector) HistoryEntry(entry HistoryEntry) (Entry, bool) {
	if display, ok := p.ContextEntry(entry); ok {
		return display, true
	}
	msg := entry.Message
	switch msg.Role {
	case llm.RoleUser:
		return EntryUser(msg.Content, time.Time{})
	case llm.RoleAssistant:
		return EntryAgent(msg.Content, msg.Reasoning, time.Time{})
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
		title := tool.Title(name, args, tool.Options{Workdir: p.workdir})
		return Tool(title, msg.Content, isError, time.Time{})
	case llm.RoleSystem, llm.RoleDeveloper:
		return EntrySystem(msg.Content, time.Time{})
	default:
		return Entry{}, false
	}
}

func (p Projector) ContextEntry(entry HistoryEntry) (Entry, bool) {
	if entry.EventType != ContextAdded {
		return Entry{}, false
	}
	switch entry.ContextKind {
	case ContextKindSummary, ContextKindWorkingSet, ContextKindBootstrap:
		return EntrySystem(entry.Message.Content, time.Time{})
	default:
		return Entry{}, false
	}
}

func (p Projector) SnapshotEntries(snapshot CompactionSnapshot) []Entry {
	entries := make(
		[]Entry,
		0,
		max(len(snapshot.Entries), len(snapshot.Messages)),
	)
	if len(snapshot.Entries) > 0 {
		for _, entry := range snapshot.Entries {
			if display, ok := p.HistoryEntry(entry); ok {
				entries = append(entries, display)
			}
		}
		return Normalize(entries)
	}
	for _, msg := range snapshot.Messages {
		if display, ok := p.HistoryEntry(HistoryEntry{Message: msg}); ok {
			entries = append(entries, display)
		}
	}
	return Normalize(entries)
}

func emptyAgent(entry Entry) bool {
	return entry.Role == RoleAgent &&
		strings.TrimSpace(entry.Content) == "" &&
		strings.TrimSpace(entry.Reasoning) == ""
}
