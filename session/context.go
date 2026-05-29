package session

import (
	"context"

	"github.com/nijaru/ion/llm"
)

// ContextKind identifies model-visible context that is not conversational
// transcript and must not be treated as privileged instructions.
type ContextKind string

const (
	ContextKindGeneric    ContextKind = "generic"
	ContextKindBootstrap  ContextKind = "bootstrap"
	ContextKindHarness    ContextKind = "harness"
	ContextKindSummary    ContextKind = "summary"
	ContextKindWorkingSet ContextKind = "working_set"
)

// ContextPlacement controls where durable context is placed in the model-visible
// request relative to the conversational transcript.
type ContextPlacement string

const (
	// ContextPlacementHistory replays context in the ordinary history suffix.
	ContextPlacementHistory ContextPlacement = "history"
	// ContextPlacementPrefix replays stable context before the transcript so
	// common prompt-cache prefixes survive ordinary turn growth.
	ContextPlacementPrefix ContextPlacement = "prefix"
)

// ContextEntry is durable, model-visible context. It is replayed into prompt
// history as ordinary user-role context, never as a system/developer message.
type ContextEntry struct {
	Kind      ContextKind      `json:"kind,omitzero"`
	Placement ContextPlacement `json:"placement,omitzero"`
	Content   string           `json:"content"`
}

// NewContext creates a context-added event.
func NewContext(sessionID string, entry ContextEntry) Event {
	normalizeContextEntry(&entry)
	return NewEvent(sessionID, ContextAdded, entry)
}

// AppendContext appends durable model-visible context to the session.
func (s *Session) AppendContext(ctx context.Context, entry ContextEntry) error {
	return s.Append(ctx, NewContext(s.ID(), entry))
}

func (e *Event) ensureContextEntry() (*ContextEntry, error) {
	if e.Type != ContextAdded {
		return nil, nil
	}

	var entry ContextEntry
	if err := e.UnmarshalData(&entry); err != nil {
		return nil, err
	}
	normalizeContextEntry(&entry)
	return &entry, nil
}

func normalizeContextEntry(entry *ContextEntry) {
	if entry.Kind == "" {
		entry.Kind = ContextKindGeneric
	}
	if entry.Placement != "" {
		return
	}
	switch entry.Kind {
	case ContextKindBootstrap, ContextKindHarness, ContextKindSummary, ContextKindWorkingSet:
		entry.Placement = ContextPlacementPrefix
	default:
		entry.Placement = ContextPlacementHistory
	}
}

func contextEntryMessage(entry ContextEntry) llm.Message {
	return llm.Message{
		Role:    llm.RoleUser,
		Content: entry.Content,
	}
}
