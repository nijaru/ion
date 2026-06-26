package session

import (
	"context"
	"database/sql"
	"os"
	"path/filepath"
	"time"

	_ "modernc.org/sqlite"
)

// SQLiteStore implements Store backed by SQLite. One row per entry,
// discriminated by type; message payload as JSON content blocks.
// Tree structure via parent_id; leaf pointer via session_meta table.
type SQLiteStore struct {
	db    *sql.DB
	meta  Metadata
	leaf  string
}

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
`

// NewSQLiteStore opens (or creates) a session store at the given path.
func NewSQLiteStore(path string, sessionID string) (*SQLiteStore, error) {
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, err
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

func (s *SQLiteStore) GetLeafID() string      { return s.leaf }
func (s *SQLiteStore) GetMetadata() Metadata   { return s.meta }
func (s *SQLiteStore) Meta() Metadata            { return s.meta }
func (s *SQLiteStore) Close() error            { return s.db.Close() }

func (s *SQLiteStore) SetLeafID(id string) error {
	_, err := s.db.Exec(
		"INSERT INTO session_meta(key,value) VALUES('leaf_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		id,
	)
	if err == nil {
		s.leaf = id
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
		"SELECT session_id, model, branch, name, summary, updated_at FROM sessions WHERE workdir = ? ORDER BY updated_at DESC",
		workdir)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var sessions []SessionInfoEntry
	for rows.Next() {
		var sid, model, branch, name, summary string
		var updatedAt int64
		if err := rows.Scan(&sid, &model, &branch, &name, &summary, &updatedAt); err != nil {
			return nil, err
		}
		sessions = append(sessions, SessionInfoEntry{
			EntryBase: EntryBase{ID: sid, Timestamp: time.Unix(updatedAt, 0)},
			Model:     model,
			Branch:    branch,
			Name:      name,
			Summary:   summary,
		})
	}
	return sessions, rows.Err()
}

// NewEphemeralCantoStore creates an in-memory CantoStore for testing.
func NewEphemeralCantoStore() (Store, error) { return NewCantoStore(":memory:") }

// NewCantoStore creates a SQLite-backed store with input history support.
// If path is a directory, "canto.db" is appended automatically.
func NewCantoStore(path string) (Store, error) {
	if path != ":memory:" {
		info, err := os.Stat(path)
		if err == nil && info.IsDir() {
			path = filepath.Join(path, "canto.db")
		}
	}
	const cantoSchema = `CREATE TABLE IF NOT EXISTS entries (
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
		session_id TEXT PRIMARY KEY,
		workdir    TEXT NOT NULL,
		model      TEXT NOT NULL DEFAULT '',
		branch     TEXT NOT NULL DEFAULT '',
		name       TEXT NOT NULL DEFAULT '',
		summary    TEXT NOT NULL DEFAULT '',
		updated_at INTEGER NOT NULL DEFAULT 0
	);
	CREATE INDEX IF NOT EXISTS idx_sessions_workdir ON sessions(workdir, updated_at);
	`

	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, err
	}
	if path == ":memory:" {
		db.SetMaxOpenConns(1)
	}
	if _, err := db.Exec(cantoSchema); err != nil {
		db.Close()
		return nil, err
	}
	return &SQLiteStore{db: db, meta: Metadata{ID: "canto"}}, nil
}

func (s *SQLiteStore) UpdateSession(ctx context.Context, info SessionInfoEntry) error {
	if s.db == nil {
		return nil
	}
	_, err := s.db.ExecContext(ctx,
		"INSERT OR REPLACE INTO sessions (session_id, workdir, model, branch, name, summary, updated_at) VALUES (?, '', ?, ?, ?, ?, ?)",
		info.ID, info.Model, info.Branch, info.Name, info.Summary, info.UpdatedAt.Unix())
	return err
}
