package session

import "time"

// Entry is a node in the session tree. The session is an append-only tree of
// entries; a Message is the dominant entry kind, but metadata changes (model
// switches, compaction markers, branch summaries) are distinct kinds.
//
// Translated from Pi's session entry model (entry.type discriminates: message,
// model_change, thinking_level_change, active_tools_change, compaction,
// branch_summary, label, session_info, custom). The interface is sealed via
// isEntry; type switches are exhaustive.
type Entry interface {
	IsEntry()
	ID() string
	ParentID() string
	When() time.Time
}

// EntryBase carries the tree linkage and timestamp shared by every entry.
type EntryBase struct {
	ID, ParentID string
	Timestamp    time.Time
}

type MessageEntry struct {
	EntryBase
	Message Message
}

type ModelChangeEntry struct {
	EntryBase
	Provider string
	ModelID  string
}

type ThinkingChangeEntry struct {
	EntryBase
	Level ThinkingLevel
}

type ToolsChangeEntry struct {
	EntryBase
	ActiveTools []string
}

type CompactionEntry struct {
	EntryBase
	Summary      string
	FirstKeptID  string
	TokensBefore int
	Details      []byte
}

type BranchSummaryEntry struct {
	EntryBase
	Summary string
	Details []byte
}

type LabelEntry struct {
	EntryBase
	TargetID string
	Label    string
}

type SessionInfoEntry struct {
	Model     string
	Branch    string
	Workdir   string
	UpdatedAt time.Time
	EntryBase
	Name        string
	Summary     string
	LastPreview string
}

// CustomEntry is an extension point for auxiliary persisted data
// (token-usage rows, status, routing decisions, subagent links).
type CustomEntry struct {
	EntryBase
	Type string
	Data []byte
}

// LeafEntry records a change in the session leaf pointer (tree navigation).
// BuildContext skips this entry — it's metadata, not a message.
// Pi equivalent: "leaf" entry kind recorded in flushPendingSessionWrites.
type LeafEntry struct {
	EntryBase
	TargetID string // the entry ID the leaf moved to
}

// CustomMessageEntry is a custom message persisted in the tree.
// Unlike CustomEntry (opaque data blob), CustomMessageEntry carries structured
// content that BuildContext projects as a CustomMessage in the message stream.
type CustomMessageEntry struct {
	EntryBase
	CustomType string
	Content    []Content
	Display    string
	Details    []byte
}

func (e *MessageEntry) IsEntry()        {}
func (e *ModelChangeEntry) IsEntry()    {}
func (e *ThinkingChangeEntry) IsEntry() {}
func (e *ToolsChangeEntry) IsEntry()    {}
func (e *CompactionEntry) IsEntry()     {}
func (e *BranchSummaryEntry) IsEntry()  {}
func (e *LabelEntry) IsEntry()          {}
func (e *SessionInfoEntry) IsEntry()    {}
func (e *CustomEntry) IsEntry()         {}
func (e *LeafEntry) IsEntry()          {}
func (e *CustomMessageEntry) IsEntry() {}

func (e *MessageEntry) ID() string        { return e.EntryBase.ID }
func (e *ModelChangeEntry) ID() string    { return e.EntryBase.ID }
func (e *ThinkingChangeEntry) ID() string { return e.EntryBase.ID }
func (e *ToolsChangeEntry) ID() string    { return e.EntryBase.ID }
func (e *CompactionEntry) ID() string     { return e.EntryBase.ID }
func (e *BranchSummaryEntry) ID() string  { return e.EntryBase.ID }
func (e *LabelEntry) ID() string          { return e.EntryBase.ID }
func (e *SessionInfoEntry) ID() string    { return e.EntryBase.ID }
func (e *CustomEntry) ID() string         { return e.EntryBase.ID }
func (e *LeafEntry) ID() string          { return e.EntryBase.ID }
func (e *CustomMessageEntry) ID() string { return e.EntryBase.ID }

func (e *MessageEntry) ParentID() string        { return e.EntryBase.ParentID }
func (e *ModelChangeEntry) ParentID() string    { return e.EntryBase.ParentID }
func (e *ThinkingChangeEntry) ParentID() string { return e.EntryBase.ParentID }
func (e *ToolsChangeEntry) ParentID() string    { return e.EntryBase.ParentID }
func (e *CompactionEntry) ParentID() string     { return e.EntryBase.ParentID }
func (e *BranchSummaryEntry) ParentID() string  { return e.EntryBase.ParentID }
func (e *LabelEntry) ParentID() string          { return e.EntryBase.ParentID }
func (e *SessionInfoEntry) ParentID() string    { return e.EntryBase.ParentID }
func (e *CustomEntry) ParentID() string         { return e.EntryBase.ParentID }
func (e *LeafEntry) ParentID() string          { return e.EntryBase.ParentID }
func (e *CustomMessageEntry) ParentID() string { return e.EntryBase.ParentID }

func (e *MessageEntry) When() time.Time        { return e.EntryBase.Timestamp }
func (e *ModelChangeEntry) When() time.Time    { return e.EntryBase.Timestamp }
func (e *ThinkingChangeEntry) When() time.Time { return e.EntryBase.Timestamp }
func (e *ToolsChangeEntry) When() time.Time    { return e.EntryBase.Timestamp }
func (e *CompactionEntry) When() time.Time     { return e.EntryBase.Timestamp }
func (e *BranchSummaryEntry) When() time.Time  { return e.EntryBase.Timestamp }
func (e *LabelEntry) When() time.Time          { return e.EntryBase.Timestamp }
func (e *SessionInfoEntry) When() time.Time    { return e.EntryBase.Timestamp }
func (e *CustomEntry) When() time.Time         { return e.EntryBase.Timestamp }
func (e *LeafEntry) When() time.Time          { return e.EntryBase.Timestamp }
func (e *CustomMessageEntry) When() time.Time { return e.EntryBase.Timestamp }

// ThinkingLevel controls reasoning effort for a turn.
type ThinkingLevel string

const (
	ThinkingOff    ThinkingLevel = "off"
	ThinkingLow    ThinkingLevel = "low"
	ThinkingMedium ThinkingLevel = "medium"
	ThinkingHigh   ThinkingLevel = "high"
)

// EntryTitle returns a display name for an entry, or empty string.
func EntryTitle(e Entry) string {
	if te, ok := e.(*TestEntry); ok {
		return te.Title
	}
	if le, ok := e.(*LabelEntry); ok {
		return le.Label
	}
	if me, ok := e.(*MessageEntry); ok {
		if tr, ok := me.Message.(*ToolResultMessage); ok {
			if tr.Title != "" {
				return tr.Title
			}
			return tr.ToolName
		}
	}
	return ""
}

// EntrySystem creates a system message entry.
func EntrySystem(content string, ts time.Time) (*MessageEntry, error) {
	return &MessageEntry{
		EntryBase: EntryBase{Timestamp: ts},
		Message:   &UserMessage{Content: []Content{TextContent{Text: content}}, Timestamp: ts},
	}, nil
}

// EntryCustom creates a custom entry for auxiliary persisted data.
func EntryCustom(customType string, data []byte, ts time.Time) (*CustomEntry, error) {
	return &CustomEntry{
		EntryBase: EntryBase{Timestamp: ts},
		Type:      customType,
		Data:      data,
	}, nil
}

func (e *SessionInfoEntry) Title() string { return e.Name }

// EntryIsError returns true if the entry is a tool result with an error.
func EntryIsError(e Entry) bool {
	if te, ok := e.(*TestEntry); ok {
		return te.IsError
	}
	if me, ok := e.(*MessageEntry); ok {
		if tr, ok := me.Message.(*ToolResultMessage); ok {
			return tr.IsError
		}
	}
	return false
}

// EntryContent returns the text content of the entry.
func EntryContent(e Entry) string { return EntryText(e) }

func EntryReasoning(e Entry) string {
	if te, ok := e.(*TestEntry); ok {
		return te.Reasoning
	}
	if me, ok := e.(*MessageEntry); ok {
		if am, ok := me.Message.(*AssistantMessage); ok {
			for _, c := range am.Content {
				if tc, ok := c.(ThinkingContent); ok {
					return tc.Text
				}
			}
		}
	}
	return ""
}

// TestEntry is a test-only type that implements Entry with flat fields.
// This makes tests simpler than constructing MessageEntry + Message.
type TestEntry struct {
	TestID       string
	TestParentID string
	Role         string
	Title        string
	Content      string
	Reasoning    string
	IsError      bool
	Timestamp    time.Time
}

func (e *TestEntry) IsEntry()         {}
func (e *TestEntry) ID() string       { return e.TestID }
func (e *TestEntry) ParentID() string { return e.TestParentID }
func (e *TestEntry) When() time.Time  { return e.Timestamp }
