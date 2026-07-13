package session

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"time"

	_ "modernc.org/sqlite"
)

// SQLiteStore implements Store backed by SQLite. One row per entry,
// discriminated by type; message payload as JSON content blocks.
// Tree structure via parent_id; leaf pointer via session_meta table.
type SQLiteStore struct {
	db   *sql.DB
	mu   sync.Mutex
	meta Metadata
	leaf string
}

var _ Store = (*SQLiteStore)(nil)

// Schema holds the SQL for creating the tables.
const Schema = `
CREATE TABLE IF NOT EXISTS entries (
	id         TEXT PRIMARY KEY,
	parent_id  TEXT NOT NULL DEFAULT '',
	type       TEXT NOT NULL,
	timestamp  INTEGER NOT NULL,
	payload    BLOB NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_entries_parent ON entries(parent_id);
CREATE INDEX IF NOT EXISTS idx_entries_type ON entries(type);

CREATE TABLE IF NOT EXISTS session_meta (
	key   TEXT PRIMARY KEY,
	value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS input_history (
	id        INTEGER PRIMARY KEY AUTOINCREMENT,
	workdir   TEXT NOT NULL,
	input     TEXT NOT NULL,
	timestamp INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_input_workdir ON input_history(workdir, timestamp);

CREATE TABLE IF NOT EXISTS sessions (
	session_id   TEXT PRIMARY KEY,
	workdir      TEXT NOT NULL,
	model        TEXT NOT NULL DEFAULT '',
	branch       TEXT NOT NULL DEFAULT '',
	name         TEXT NOT NULL DEFAULT '',
	summary      TEXT NOT NULL DEFAULT '',
	last_preview TEXT NOT NULL DEFAULT '',
	updated_at   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_sessions_workdir ON sessions(workdir, updated_at);
`

// NewSQLiteStore opens or creates the complete Ion SQLite store. If path names
// a directory, ion.db is created inside it; ":memory:" remains in-memory.
func NewSQLiteStore(path string, sessionID string) (*SQLiteStore, error) {
	if path != ":memory:" {
		info, err := os.Stat(path)
		if err == nil && info.IsDir() {
			path = filepath.Join(path, "ion.db")
		}
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, err
	}
	if path == ":memory:" {
		db.SetMaxOpenConns(1)
	}
	if _, err := db.Exec(Schema); err != nil {
		db.Close()
		return nil, err
	}
	s := &SQLiteStore{db: db, meta: Metadata{ID: sessionID}}
	if err := s.loadMeta(); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

func (s *SQLiteStore) loadMeta() error {
	// Load leaf pointer.
	var leaf sql.NullString
	if err := s.db.QueryRow("SELECT value FROM session_meta WHERE key='leaf_id'").Scan(&leaf); err != nil && err != sql.ErrNoRows {
		return err
	}
	if leaf.Valid {
		s.leaf = leaf.String
	}
	// Load session name.
	var name sql.NullString
	if err := s.db.QueryRow("SELECT value FROM session_meta WHERE key='name'").Scan(&name); err != nil && err != sql.ErrNoRows {
		return err
	}
	if name.Valid {
		s.meta.Name = name.String
	}
	return nil
}

func (s *SQLiteStore) GetLeafID() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.leaf
}

func (s *SQLiteStore) Meta() Metadata {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.meta
}

func (s *SQLiteStore) Close() error { return s.db.Close() }

func (s *SQLiteStore) SetLeafID(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, err := s.db.Exec(
		"INSERT INTO session_meta(key,value) VALUES('leaf_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		id,
	)
	if err == nil {
		s.leaf = id
	}
	return err
}

// ResumeSession validates an existing entry and makes it the current leaf.
func (s *SQLiteStore) ResumeSession(ctx context.Context, entryID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, err := s.getEntry(ctx, entryID); err != nil {
		return err
	}
	_, err := s.db.ExecContext(ctx,
		"INSERT INTO session_meta(key,value) VALUES('leaf_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		entryID,
	)
	if err == nil {
		s.leaf = entryID
	}
	return err
}

func (s *SQLiteStore) GetInputs(ctx context.Context, workdir string, n int) ([]string, error) {
	if s.db == nil {
		return nil, nil
	}
	rows, err := s.db.QueryContext(ctx,
		"SELECT input FROM input_history WHERE workdir = ? ORDER BY timestamp DESC LIMIT ?",
		workdir, n)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var inputs []string
	for rows.Next() {
		var input string
		if err := rows.Scan(&input); err != nil {
			return nil, err
		}
		inputs = append(inputs, input)
	}
	return inputs, rows.Err()
}
func (s *SQLiteStore) AddInput(ctx context.Context, workdir string, input string) error {
	if s.db == nil || input == "" {
		return nil
	}
	_, err := s.db.ExecContext(ctx,
		"INSERT INTO input_history (workdir, input, timestamp) VALUES (?, ?, ?)",
		workdir, input, time.Now().Unix())
	return err
}

func (s *SQLiteStore) ListSessions(ctx context.Context, workdir string) ([]SessionInfoEntry, error) {
	if s.db == nil {
		return nil, nil
	}
	rows, err := s.db.QueryContext(ctx,
		"SELECT session_id, workdir, model, branch, name, summary, last_preview, updated_at FROM sessions WHERE workdir = ? ORDER BY updated_at DESC",
		workdir)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var sessions []SessionInfoEntry
	for rows.Next() {
		var sid, wd, model, branch, name, summary, preview string
		var updatedAt int64
		if err := rows.Scan(&sid, &wd, &model, &branch, &name, &summary, &preview, &updatedAt); err != nil {
			return nil, err
		}
		sessions = append(sessions, SessionInfoEntry{
			EntryBase:   EntryBase{ID: sid, Timestamp: time.Unix(updatedAt, 0)},
			Workdir:     wd,
			Model:       model,
			Branch:      branch,
			Name:        name,
			Summary:     summary,
			LastPreview: preview,
		})
	}
	return sessions, rows.Err()
}

func (s *SQLiteStore) UpdateSession(ctx context.Context, info SessionInfoEntry) error {
	if s.db == nil {
		return nil
	}
	if info.ID() == "" {
		return fmt.Errorf("session id is required")
	}
	updatedAt := info.UpdatedAt
	if updatedAt.IsZero() {
		updatedAt = time.Now()
	}
	_, err := s.db.ExecContext(ctx,
		"INSERT OR REPLACE INTO sessions (session_id, workdir, model, branch, name, summary, last_preview, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
		info.ID(), info.Workdir, info.Model, info.Branch, info.Name, info.Summary, info.LastPreview, updatedAt.Unix())
	return err
}

// Append persists an entry to the store.
func (s *SQLiteStore) Append(ctx context.Context, entry Entry) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.append(ctx, entry)
}

// append is the internal (unlocked) entry writer.
func (s *SQLiteStore) append(ctx context.Context, entry Entry) (string, error) {
	id := entry.ID()
	parentID := entry.ParentID()
	ts := entry.When().UnixMilli()
	typ, payload, err := encodeEntry(entry)
	if err != nil {
		return "", err
	}
	_, err = s.db.ExecContext(ctx,
		"INSERT INTO entries(id,parent_id,type,timestamp,payload) VALUES(?,?,?,?,?)",
		id, parentID, typ, ts, payload,
	)
	return id, err
}

// AppendLeafEntry appends an entry and atomically updates the leaf pointer.
// Both operations happen under one lock so concurrent AppendLeaf calls cannot
// interleave GetLeafID → Append → SetLeafID.
func (s *SQLiteStore) AppendLeafEntry(ctx context.Context, entry Entry) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	id, err := s.append(ctx, entry)
	if err != nil {
		return "", err
	}
	s.leaf = id
	// Persist the leaf pointer so the branch is reachable after restart.
	// Pi writes the leaf on every append; without this, a linear session (no
	// MoveTo) is unreachable after reopening the store (loadMeta reads it).
	if _, err := s.db.ExecContext(ctx,
		"INSERT INTO session_meta(key,value) VALUES('leaf_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		id); err != nil {
		return id, err
	}
	return id, nil
}

// AppendBatch persists multiple entries atomically using a SQLite transaction.
func (s *SQLiteStore) AppendBatch(ctx context.Context, entries []Entry) ([]string, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, fmt.Errorf("begin transaction: %w", err)
	}
	defer tx.Rollback()

	ids := make([]string, len(entries))
	for i, entry := range entries {
		id := entry.ID()
		parentID := entry.ParentID()
		ts := entry.When().UnixMilli()
		typ, payload, err := encodeEntry(entry)
		if err != nil {
			return nil, err
		}
		_, err = tx.ExecContext(ctx,
			"INSERT INTO entries(id,parent_id,type,timestamp,payload) VALUES(?,?,?,?,?)",
			id, parentID, typ, ts, payload,
		)
		if err != nil {
			return nil, err
		}
		ids[i] = id
	}

	if err := tx.Commit(); err != nil {
		return nil, fmt.Errorf("commit: %w", err)
	}
	return ids, nil
}

// GetEntry returns a single entry by ID.
func (s *SQLiteStore) GetEntry(ctx context.Context, id string) (Entry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.getEntry(ctx, id)
}

// getEntry is the internal (unlocked) entry reader.
func (s *SQLiteStore) getEntry(ctx context.Context, id string) (Entry, error) {
	row := s.db.QueryRowContext(ctx, "SELECT id,parent_id,type,timestamp,payload FROM entries WHERE id=?", id)
	return scanEntry(row)
}

// Branch returns entries from root to the current leaf.
func (s *SQLiteStore) Branch(ctx context.Context) ([]Entry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
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
		e, err := s.getEntry(ctx, id)
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

	// Leaf fields
	LeafTargetID string `json:"leaf_target_id,omitempty"`

	// CustomMessage fields
	CustomMessageType    string          `json:"custom_message_type,omitempty"`
	CustomMessageContent json.RawMessage `json:"custom_message_content,omitempty"`
	CustomMessageDisplay string          `json:"custom_message_display,omitempty"`
	CustomMessageDetails json.RawMessage `json:"custom_message_details,omitempty"`
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
	case *LeafEntry:
		typ = "leaf"
		p.LeafTargetID = e.TargetID
	case *CustomMessageEntry:
		typ = "custom_message"
		p.CustomMessageType = e.CustomType
		p.CustomMessageDisplay = e.Display
		p.CustomMessageDetails = e.Details
		contentBytes, err := json.Marshal(e.Content)
		if err != nil {
			return "", nil, fmt.Errorf("marshal custom_message content: %w", err)
		}
		p.CustomMessageContent = contentBytes
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
		id       string
		parentID string
		typ      string
		ts       int64
		payload  []byte
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
	case "leaf":
		return &LeafEntry{EntryBase: base, TargetID: p.LeafTargetID}, nil
	case "custom_message":
		// Unmarshal content with the same typed dispatch as other messages
		// (text/thinking/image/tool_call) so non-text blocks round-trip. The
		// old text-only heuristic dropped images and corrupted other blocks.
		var rawContent []json.RawMessage
		if err := json.Unmarshal(p.CustomMessageContent, &rawContent); err != nil {
			return nil, fmt.Errorf("unmarshal custom_message content: %w", err)
		}
		content, err := unmarshalContentSlice(rawContent)
		if err != nil {
			return nil, fmt.Errorf("unmarshal custom_message content: %w", err)
		}
		return &CustomMessageEntry{EntryBase: base, CustomType: p.CustomMessageType, Content: content, Display: p.CustomMessageDisplay, Details: p.CustomMessageDetails}, nil
	default:
		return nil, fmt.Errorf("unknown entry type %q", typ)
	}
}
