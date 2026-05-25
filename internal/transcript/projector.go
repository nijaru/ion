package transcript

import (
	"strings"
	"time"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/tooldisplay"
)

type Projector struct {
	workdir string
}

func New(workdir string) Projector {
	return Projector{workdir: workdir}
}

func WithTimestamp(entry ionsession.Entry, timestamp time.Time) ionsession.Entry {
	if !timestamp.IsZero() {
		entry.Timestamp = timestamp.UTC()
	}
	return entry
}

func SetTimestamp(entry *ionsession.Entry, timestamp time.Time) {
	if entry != nil && !timestamp.IsZero() {
		entry.Timestamp = timestamp.UTC()
	}
}

func Normalize(entries []ionsession.Entry) []ionsession.Entry {
	normalized := make([]ionsession.Entry, 0, len(entries))
	for _, entry := range entries {
		if emptyAgent(entry) {
			continue
		}
		normalized = append(normalized, entry)
	}
	return normalized
}

func User(content string, timestamp time.Time) (ionsession.Entry, bool) {
	if strings.TrimSpace(content) == "" {
		return ionsession.Entry{}, false
	}
	return WithTimestamp(ionsession.Entry{
		Role:    ionsession.User,
		Content: content,
	}, timestamp), true
}

func Agent(content, reasoning string, timestamp time.Time) (ionsession.Entry, bool) {
	entry := WithTimestamp(ionsession.Entry{
		Role:      ionsession.Agent,
		Content:   content,
		Reasoning: reasoning,
	}, timestamp)
	if emptyAgent(entry) {
		return ionsession.Entry{}, false
	}
	return entry, true
}

func System(content string, timestamp time.Time) (ionsession.Entry, bool) {
	return WithTimestamp(ionsession.Entry{
		Role:    ionsession.System,
		Content: content,
	}, timestamp), true
}

func Tool(title, content string, isError bool, timestamp time.Time) (ionsession.Entry, bool) {
	if title == "" {
		title = "tool"
	}
	return WithTimestamp(ionsession.Entry{
		Role:    ionsession.Tool,
		Title:   title,
		Content: content,
		IsError: isError,
	}, timestamp), true
}

func Subagent(title, content string, isError bool, timestamp time.Time) (ionsession.Entry, bool) {
	return WithTimestamp(ionsession.Entry{
		Role:    ionsession.Subagent,
		Title:   title,
		Content: content,
		IsError: isError,
	}, timestamp), true
}

func (p Projector) HistoryEntry(entry csession.HistoryEntry) (ionsession.Entry, bool) {
	if display, ok := p.ContextEntry(entry); ok {
		return display, true
	}
	msg := entry.Message
	switch msg.Role {
	case llm.RoleUser:
		return User(msg.Content, time.Time{})
	case llm.RoleAssistant:
		return Agent(msg.Content, msg.Reasoning, time.Time{})
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
		title := tooldisplay.Title(name, args, tooldisplay.Options{Workdir: p.workdir})
		return Tool(title, msg.Content, isError, time.Time{})
	case llm.RoleSystem, llm.RoleDeveloper:
		return System(msg.Content, time.Time{})
	default:
		return ionsession.Entry{}, false
	}
}

func (p Projector) ContextEntry(entry csession.HistoryEntry) (ionsession.Entry, bool) {
	if entry.EventType != csession.ContextAdded {
		return ionsession.Entry{}, false
	}
	switch entry.ContextKind {
	case csession.ContextKindSummary, csession.ContextKindWorkingSet, csession.ContextKindBootstrap:
		return System(entry.Message.Content, time.Time{})
	default:
		return ionsession.Entry{}, false
	}
}

func (p Projector) SnapshotEntries(snapshot csession.CompactionSnapshot) []ionsession.Entry {
	entries := make(
		[]ionsession.Entry,
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
		if display, ok := p.HistoryEntry(csession.HistoryEntry{Message: msg}); ok {
			entries = append(entries, display)
		}
	}
	return Normalize(entries)
}

func emptyAgent(entry ionsession.Entry) bool {
	return entry.Role == ionsession.Agent &&
		strings.TrimSpace(entry.Content) == "" &&
		strings.TrimSpace(entry.Reasoning) == ""
}
