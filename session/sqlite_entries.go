package session

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"
)

// Append persists an entry to the store.
func (s *SQLiteStore) Append(ctx context.Context, entry Entry) error {
	id := entry.ID()
	parentID := entry.ParentID()
	ts := entry.When().UnixMilli()
	typ, payload, err := encodeEntry(entry)
	if err != nil {
		return err
	}
	_, err = s.db.ExecContext(ctx,
		"INSERT INTO entries(id,parent_id,type,timestamp,payload) VALUES(?,?,?,?,?)",
		id, parentID, typ, ts, payload,
	)
	return err
}

// GetEntry returns a single entry by ID.
func (s *SQLiteStore) GetEntry(ctx context.Context, id string) (Entry, error) {
	row := s.db.QueryRowContext(ctx, "SELECT id,parent_id,type,timestamp,payload FROM entries WHERE id=?", id)
	return scanEntry(row)
}

// Branch returns entries from root to the current leaf.
func (s *SQLiteStore) Branch(ctx context.Context) ([]Entry, error) {
	if s.leaf == "" {
		return nil, nil
	}
	// Walk parent_id chain from leaf to root, then reverse.
	var ids []string
	id := s.leaf
	for id != "" {
		ids = append(ids, id)
		var parentID string
		err := s.db.QueryRowContext(ctx, "SELECT parent_id FROM entries WHERE id=?", id).Scan(&parentID)
		if err == sql.ErrNoRows {
			break
		}
		if err != nil {
			return nil, err
		}
		id = parentID
	}
	// Reverse to root-to-leaf order.
	for i, j := 0, len(ids)-1; i < j; i, j = i+1, j-1 {
		ids[i], ids[j] = ids[j], ids[i]
	}
	entries := make([]Entry, 0, len(ids))
	for _, id := range ids {
		e, err := s.GetEntry(ctx, id)
		if err != nil {
			return nil, err
		}
		entries = append(entries, e)
	}
	return entries, nil
}

// Entries returns all entries in the session.
func (s *SQLiteStore) Entries(ctx context.Context) ([]Entry, error) {
	rows, err := s.db.QueryContext(ctx, "SELECT id,parent_id,type,timestamp,payload FROM entries ORDER BY timestamp ASC")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var entries []Entry
	for rows.Next() {
		e, err := scanEntry(rows)
		if err != nil {
			return nil, err
		}
		entries = append(entries, e)
	}
	return entries, rows.Err()
}

// --- encoding/decoding ---

type entryPayload struct {
	// Message fields — stored as raw JSON (Message is an interface)
	Message json.RawMessage `json:"message,omitempty"`

	// ModelChange fields
	Provider string `json:"provider,omitempty"`
	ModelID  string `json:"model_id,omitempty"`

	// ThinkingChange fields
	Level string `json:"level,omitempty"`

	// ToolsChange fields
	ActiveTools []string `json:"active_tools,omitempty"`

	// Compaction fields
	Summary      string `json:"summary,omitempty"`
	FirstKeptID  string `json:"first_kept_id,omitempty"`
	TokensBefore int    `json:"tokens_before,omitempty"`

	// BranchSummary fields
	Details []byte `json:"details,omitempty"`

	// Label fields
	TargetID string `json:"target_id,omitempty"`
	Label    string `json:"label,omitempty"`

	// SessionInfo fields
	Name string `json:"name,omitempty"`

	// Custom fields
	CustomType string          `json:"custom_type,omitempty"`
	CustomData json.RawMessage `json:"custom_data,omitempty"`
}

// encodeEntry serializes an Entry to (type string, JSON payload bytes, error).
func encodeEntry(e Entry) (string, []byte, error) {
	var typ string
	var p entryPayload
	switch e := e.(type) {
	case *MessageEntry:
		typ = "message"
		b, err := json.Marshal(e.Message)
		if err != nil {
			return "", nil, fmt.Errorf("marshal message: %w", err)
		}
		p.Message = b
	case *ModelChangeEntry:
		typ = "model_change"
		p.Provider = e.Provider
		p.ModelID = e.ModelID
	case *ThinkingChangeEntry:
		typ = "thinking_change"
		p.Level = string(e.Level)
	case *ToolsChangeEntry:
		typ = "tools_change"
		p.ActiveTools = e.ActiveTools
	case *CompactionEntry:
		typ = "compaction"
		p.Summary = e.Summary
		p.FirstKeptID = e.FirstKeptID
		p.TokensBefore = e.TokensBefore
		p.Details = e.Details
	case *BranchSummaryEntry:
		typ = "branch_summary"
		p.Summary = e.Summary
		p.Details = e.Details
	case *LabelEntry:
		typ = "label"
		p.TargetID = e.TargetID
		p.Label = e.Label
	case *SessionInfoEntry:
		typ = "session_info"
		p.Name = e.Name
	case *CustomEntry:
		typ = "custom"
		p.CustomType = e.Type
		p.CustomData = e.Data
	default:
		return "", nil, fmt.Errorf("unknown entry type %T", e)
	}
	b, err := json.Marshal(p)
	return typ, b, err
}

// scannable is the interface satisfied by both *sql.Row and *sql.Rows.
type scannable interface {
	Scan(dest ...any) error
}

func scanEntry(s scannable) (Entry, error) {
	var (
		id        string
		parentID  string
		typ       string
		ts        int64
		payload   []byte
	)
	if err := s.Scan(&id, &parentID, &typ, &ts, &payload); err != nil {
		return nil, err
	}
	base := EntryBase{ID: id, ParentID: parentID, Timestamp: time.UnixMilli(ts)}
	var p entryPayload
	if err := json.Unmarshal(payload, &p); err != nil {
		return nil, err
	}
	switch typ {
	case "message":
		if p.Message == nil {
			return nil, fmt.Errorf("message entry missing message payload")
		}
		msg, err := unmarshalMessage(p.Message)
		if err != nil {
			return nil, fmt.Errorf("unmarshal message: %w", err)
		}
		return &MessageEntry{EntryBase: base, Message: msg}, nil
	case "model_change":
		return &ModelChangeEntry{EntryBase: base, Provider: p.Provider, ModelID: p.ModelID}, nil
	case "thinking_change":
		return &ThinkingChangeEntry{EntryBase: base, Level: ThinkingLevel(p.Level)}, nil
	case "tools_change":
		return &ToolsChangeEntry{EntryBase: base, ActiveTools: p.ActiveTools}, nil
	case "compaction":
		return &CompactionEntry{EntryBase: base, Summary: p.Summary, FirstKeptID: p.FirstKeptID, TokensBefore: p.TokensBefore, Details: p.Details}, nil
	case "branch_summary":
		return &BranchSummaryEntry{EntryBase: base, Summary: p.Summary, Details: p.Details}, nil
	case "label":
		return &LabelEntry{EntryBase: base, TargetID: p.TargetID, Label: p.Label}, nil
	case "session_info":
		return &SessionInfoEntry{EntryBase: base, Name: p.Name}, nil
	case "custom":
		return &CustomEntry{EntryBase: base, Type: p.CustomType, Data: p.CustomData}, nil
	default:
		return nil, fmt.Errorf("unknown entry type %q", typ)
	}
}
