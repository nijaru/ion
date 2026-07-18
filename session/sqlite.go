package session

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"golang.org/x/sys/unix"
	_ "modernc.org/sqlite"
)

// SQLiteStore implements Store backed by SQLite. One row per entry,
// discriminated by type; message payload as JSON content blocks.
// Tree structure via parent_id; leaf pointer via session_meta table.
type SQLiteStore struct {
	db        *sql.DB
	mu        sync.RWMutex
	meta      Metadata
	leaf      string
	closed    bool
	closeErr  error
	closeOnce sync.Once
	lockFile  *os.File
}

var _ Store = (*SQLiteStore)(nil)

const currentSchemaVersion = 3

const sessionLockWait = 500 * time.Millisecond

var (
	ErrSessionClosed     = errors.New("session store is closed")
	ErrSessionBusy       = errors.New("session store is busy")
	ErrUnsupportedSchema = errors.New("unsupported session schema")
	ErrCorruptSession    = errors.New("corrupt session store")
	ErrTurnNotFound      = errors.New("turn not found")
	ErrTurnState         = errors.New("invalid turn state")
)

// Schema holds the SQL for creating the tables.
const Schema = `
CREATE TABLE IF NOT EXISTS entries (
	id         TEXT PRIMARY KEY,
	parent_id  TEXT NOT NULL DEFAULT '',
	type       TEXT NOT NULL,
	timestamp  INTEGER NOT NULL,
	sequence   INTEGER NOT NULL DEFAULT 0,
	turn_id    TEXT,
	payload    BLOB NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_entries_parent ON entries(parent_id);
CREATE INDEX IF NOT EXISTS idx_entries_type ON entries(type);
CREATE INDEX IF NOT EXISTS idx_entries_sequence ON entries(sequence);
CREATE INDEX IF NOT EXISTS idx_entries_turn ON entries(turn_id);

CREATE TABLE IF NOT EXISTS session_meta (
	key   TEXT PRIMARY KEY,
	value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS turns (
	turn_id    TEXT PRIMARY KEY,
	sequence   INTEGER NOT NULL,
	state      TEXT NOT NULL,
	leaf_id    TEXT NOT NULL DEFAULT '',
	input      TEXT NOT NULL DEFAULT '',
	context_id TEXT NOT NULL DEFAULT '',
	started_at INTEGER NOT NULL,
	ended_at   INTEGER NOT NULL DEFAULT 0,
	error      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_turns_state ON turns(state, sequence);

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
	var err error
	var lockFile *os.File
	if path != ":memory:" {
		lockFile, err = acquireSessionLock(path)
		if err != nil {
			return nil, err
		}
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000&_txlock=immediate")
	if err != nil {
		if lockFile != nil {
			_ = releaseSessionLock(lockFile)
		}
		return nil, err
	}
	// Serialize SQLite connections: the harness and TUI may persist auxiliary
	// entries concurrently, and a single connection avoids SQLITE_BUSY races.
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	if err := migrateSchema(context.Background(), db, path); err != nil {
		db.Close()
		if lockFile != nil {
			_ = releaseSessionLock(lockFile)
		}
		return nil, err
	}
	if err := recoverInterruptedTurns(context.Background(), db); err != nil {
		db.Close()
		if lockFile != nil {
			_ = releaseSessionLock(lockFile)
		}
		return nil, err
	}
	s := &SQLiteStore{db: db, lockFile: lockFile, meta: Metadata{ID: sessionID}}
	if err := s.loadMeta(); err != nil {
		db.Close()
		if lockFile != nil {
			_ = releaseSessionLock(lockFile)
		}
		return nil, err
	}
	if err := s.validateLoadedState(); err != nil {
		db.Close()
		if lockFile != nil {
			_ = releaseSessionLock(lockFile)
		}
		return nil, err
	}
	return s, nil
}

func acquireSessionLock(path string) (*os.File, error) {
	lockFile, err := os.OpenFile(path+".lock", os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open session lock: %w", err)
	}
	deadline := time.Now().Add(sessionLockWait)
	for {
		err = unix.Flock(int(lockFile.Fd()), unix.LOCK_EX|unix.LOCK_NB)
		if err == nil {
			return lockFile, nil
		}
		if !errors.Is(err, unix.EWOULDBLOCK) && !errors.Is(err, unix.EAGAIN) {
			_ = lockFile.Close()
			return nil, fmt.Errorf("acquire session lock: %w", err)
		}
		if time.Now().After(deadline) {
			_ = lockFile.Close()
			return nil, fmt.Errorf("%w: %s (waited %s)", ErrSessionBusy, path, sessionLockWait)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func releaseSessionLock(lockFile *os.File) error {
	if lockFile == nil {
		return nil
	}
	return errors.Join(unix.Flock(int(lockFile.Fd()), unix.LOCK_UN), lockFile.Close())
}

// migrateSchema upgrades the store under one immediate SQLite transaction.
// Version zero is the pre-versioned Ion schema; it is upgraded in place after
// verifying each additive change. Unknown newer schemas are never opened.
func migrateSchema(ctx context.Context, db *sql.DB, path string) error {
	ctx = normalizeContext(ctx)
	var version int
	if err := db.QueryRowContext(ctx, "PRAGMA user_version").Scan(&version); err != nil {
		return fmt.Errorf("read schema version: %w", err)
	}
	if version > currentSchemaVersion {
		return fmt.Errorf("%w: found %d, supported through %d", ErrUnsupportedSchema, version, currentSchemaVersion)
	}
	if version < currentSchemaVersion && path != ":memory:" {
		if err := backupBeforeMigration(ctx, db, path, version); err != nil {
			return err
		}
	}
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return classifySQLiteError("begin schema migration", err)
	}
	rollback := func(err error) error {
		_ = tx.Rollback()
		return err
	}

	if err := tx.QueryRowContext(ctx, "PRAGMA user_version").Scan(&version); err != nil {
		return rollback(fmt.Errorf("read schema version: %w", err))
	}
	if version > currentSchemaVersion {
		return rollback(fmt.Errorf("%w: found %d, supported through %d", ErrUnsupportedSchema, version, currentSchemaVersion))
	}
	if err := ensureBaseSchema(ctx, tx); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "entries", "sequence", "INTEGER NOT NULL DEFAULT 0"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "entries", "turn_id", "TEXT"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "turns", "input", "TEXT NOT NULL DEFAULT ''"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "turns", "context_id", "TEXT NOT NULL DEFAULT ''"); err != nil {
		return rollback(err)
	}
	if _, err := tx.ExecContext(ctx, `
		CREATE INDEX IF NOT EXISTS idx_entries_parent ON entries(parent_id);
		CREATE INDEX IF NOT EXISTS idx_entries_type ON entries(type);
		CREATE INDEX IF NOT EXISTS idx_entries_sequence ON entries(sequence);
		CREATE INDEX IF NOT EXISTS idx_entries_turn ON entries(turn_id);
		CREATE INDEX IF NOT EXISTS idx_turns_state ON turns(state, sequence);
	`); err != nil {
		return rollback(fmt.Errorf("create session indexes: %w", err))
	}
	if _, err := tx.ExecContext(ctx, "UPDATE entries SET sequence = rowid WHERE sequence = 0"); err != nil {
		return rollback(fmt.Errorf("backfill entry sequence: %w", err))
	}
	if _, err := tx.ExecContext(ctx, fmt.Sprintf("PRAGMA user_version = %d", currentSchemaVersion)); err != nil {
		return rollback(fmt.Errorf("write schema version: %w", err))
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit schema migration", err)
	}
	return nil
}

func backupBeforeMigration(ctx context.Context, db *sql.DB, path string, version int) error {
	var tables int
	if err := db.QueryRowContext(ctx,
		"SELECT count(*) FROM sqlite_master WHERE type = 'table'").Scan(&tables); err != nil {
		return fmt.Errorf("inspect database before migration: %w", err)
	}
	if tables == 0 {
		return nil
	}
	backupPath := fmt.Sprintf("%s.pre-migration-v%d", path, version)
	if _, err := os.Stat(backupPath); err == nil {
		return nil
	} else if !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("inspect migration backup: %w", err)
	}
	if _, err := db.ExecContext(ctx, "PRAGMA wal_checkpoint(TRUNCATE)"); err != nil {
		return classifySQLiteError("checkpoint database before migration", err)
	}
	source, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("open database for migration backup: %w", err)
	}
	defer source.Close()
	backup, err := os.OpenFile(backupPath, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return fmt.Errorf("create migration backup: %w", err)
	}
	keepBackup := false
	defer func() {
		if !keepBackup {
			_ = backup.Close()
			_ = os.Remove(backupPath)
		}
	}()
	if _, err := io.Copy(backup, source); err != nil {
		return fmt.Errorf("copy migration backup: %w", err)
	}
	if err := backup.Sync(); err != nil {
		return fmt.Errorf("sync migration backup: %w", err)
	}
	if err := backup.Close(); err != nil {
		return fmt.Errorf("close migration backup: %w", err)
	}
	keepBackup = true
	return nil
}

func ensureBaseSchema(ctx context.Context, tx *sql.Tx) error {
	statements := []string{
		`CREATE TABLE IF NOT EXISTS entries (
			id TEXT PRIMARY KEY,
			parent_id TEXT NOT NULL DEFAULT '',
			type TEXT NOT NULL,
			timestamp INTEGER NOT NULL,
			payload BLOB NOT NULL DEFAULT '{}'
		)`,
		`CREATE TABLE IF NOT EXISTS session_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)`,
		`CREATE TABLE IF NOT EXISTS turns (
			turn_id TEXT PRIMARY KEY,
			sequence INTEGER NOT NULL,
			state TEXT NOT NULL,
			leaf_id TEXT NOT NULL DEFAULT '',
			input TEXT NOT NULL DEFAULT '',
			context_id TEXT NOT NULL DEFAULT '',
			started_at INTEGER NOT NULL,
			ended_at INTEGER NOT NULL DEFAULT 0,
			error TEXT NOT NULL DEFAULT ''
		)`,
		`CREATE TABLE IF NOT EXISTS input_history (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			workdir TEXT NOT NULL,
			input TEXT NOT NULL,
			timestamp INTEGER NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS sessions (
			session_id TEXT PRIMARY KEY,
			workdir TEXT NOT NULL,
			model TEXT NOT NULL DEFAULT '',
			branch TEXT NOT NULL DEFAULT '',
			name TEXT NOT NULL DEFAULT '',
			summary TEXT NOT NULL DEFAULT '',
			last_preview TEXT NOT NULL DEFAULT '',
			updated_at INTEGER NOT NULL DEFAULT 0
		)`,
	}
	for _, statement := range statements {
		if _, err := tx.ExecContext(ctx, statement); err != nil {
			return fmt.Errorf("create session schema: %w", err)
		}
	}
	return nil
}

func ensureColumn(ctx context.Context, tx *sql.Tx, table, column, definition string) error {
	rows, err := tx.QueryContext(ctx, "PRAGMA table_info("+table+")")
	if err != nil {
		return fmt.Errorf("inspect %s schema: %w", table, err)
	}
	defer rows.Close()
	found := false
	for rows.Next() {
		var cid int
		var name, typ string
		var notNull, pk int
		var defaultValue sql.NullString
		if err := rows.Scan(&cid, &name, &typ, &notNull, &defaultValue, &pk); err != nil {
			return fmt.Errorf("inspect %s column: %w", table, err)
		}
		if name == column {
			found = true
		}
	}
	if err := rows.Err(); err != nil {
		return fmt.Errorf("inspect %s columns: %w", table, err)
	}
	if found {
		return nil
	}
	if _, err := tx.ExecContext(ctx, "ALTER TABLE "+table+" ADD COLUMN "+column+" "+definition); err != nil {
		return fmt.Errorf("add %s.%s: %w", table, column, err)
	}
	return nil
}

func classifySQLiteError(operation string, err error) error {
	if err == nil {
		return nil
	}
	lower := strings.ToLower(err.Error())
	if strings.Contains(lower, "busy") || strings.Contains(lower, "locked") {
		return fmt.Errorf("%w: %s: %v", ErrSessionBusy, operation, err)
	}
	return fmt.Errorf("%s: %w", operation, err)
}

func (s *SQLiteStore) ensureOpenLocked() error {
	if s.closed || s.db == nil {
		return ErrSessionClosed
	}
	return nil
}

func (s *SQLiteStore) beginWriteLocked(ctx context.Context) (*sql.Tx, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, classifySQLiteError("begin session write", err)
	}
	return tx, nil
}

func setLeafTx(ctx context.Context, tx *sql.Tx, id string) error {
	if _, err := tx.ExecContext(ctx,
		"INSERT INTO session_meta(key,value) VALUES('leaf_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		id,
	); err != nil {
		return fmt.Errorf("persist leaf pointer: %w", err)
	}
	return nil
}

func (s *SQLiteStore) getVisibleEntryTx(ctx context.Context, tx *sql.Tx, id string) (Entry, error) {
	row := tx.QueryRowContext(ctx, `
		SELECT e.id, e.parent_id, e.type, e.timestamp, e.payload
		FROM entries e
		WHERE e.id = ?
		  AND (e.turn_id IS NULL OR EXISTS (
			SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
		  ))`, id)
	return scanEntry(row)
}

func nullableString(value string) any {
	if value == "" {
		return nil
	}
	return value
}

// nextSequenceTx allocates one monotonic session sequence inside the caller's
// transaction. It is shared by entries and turn records so replay ordering is
// deterministic even when a turn has no visible messages.
func nextSequenceTx(ctx context.Context, tx *sql.Tx) (int64, error) {
	var stored sql.NullString
	err := tx.QueryRowContext(ctx, "SELECT value FROM session_meta WHERE key='next_sequence'").Scan(&stored)
	if err != nil && err != sql.ErrNoRows {
		return 0, fmt.Errorf("read session sequence: %w", err)
	}
	var next int64
	if stored.Valid && stored.String != "" {
		next, err = strconv.ParseInt(stored.String, 10, 64)
		if err != nil || next < 1 {
			return 0, fmt.Errorf("%w: invalid next sequence %q", ErrCorruptSession, stored.String)
		}
	} else {
		if err := tx.QueryRowContext(ctx, `
			SELECT COALESCE(MAX(sequence), 0) + 1
			FROM (
				SELECT sequence FROM entries
				UNION ALL
				SELECT sequence FROM turns
			)`).Scan(&next); err != nil {
			return 0, fmt.Errorf("derive session sequence: %w", err)
		}
		if next < 1 {
			next = 1
		}
	}
	if _, err := tx.ExecContext(ctx,
		"INSERT INTO session_meta(key,value) VALUES('next_sequence',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		strconv.FormatInt(next+1, 10),
	); err != nil {
		return 0, fmt.Errorf("advance session sequence: %w", err)
	}
	return next, nil
}

func (s *SQLiteStore) loadMeta() error {
	// Session identity is durable and never follows the selected leaf.
	var storedID sql.NullString
	if err := s.db.QueryRow("SELECT value FROM session_meta WHERE key='session_id'").Scan(&storedID); err != nil && err != sql.ErrNoRows {
		return err
	}
	if storedID.Valid && storedID.String != "" {
		s.meta.ID = storedID.String
	} else {
		if s.meta.ID == "" {
			s.meta.ID = newID()
		}
		if _, err := s.db.Exec(
			"INSERT INTO session_meta(key,value) VALUES('session_id',?) ON CONFLICT(key) DO NOTHING",
			s.meta.ID,
		); err != nil {
			return fmt.Errorf("persist session identity: %w", err)
		}
	}

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

func (s *SQLiteStore) validateLoadedState() error {
	if s.leaf == "" {
		return nil
	}
	var id string
	err := s.db.QueryRow(`
		SELECT e.id FROM entries e
		WHERE e.id = ? AND (e.turn_id IS NULL OR EXISTS (
			SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
		))`, s.leaf).Scan(&id)
	if err == sql.ErrNoRows {
		return fmt.Errorf("%w: leaf %q does not identify visible entry", ErrCorruptSession, s.leaf)
	}
	if err != nil {
		return fmt.Errorf("validate session leaf: %w", err)
	}
	return nil
}

func (s *SQLiteStore) GetLeafID() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.leaf
}

func (s *SQLiteStore) Meta() Metadata {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.meta
}

func (s *SQLiteStore) Close() error {
	s.closeOnce.Do(func() {
		s.mu.Lock()
		s.closed = true
		db := s.db
		s.mu.Unlock()
		s.closeErr = errors.Join(db.Close(), releaseSessionLock(s.lockFile))
	})
	return s.closeErr
}

func (s *SQLiteStore) SetLeafID(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return err
	}
	tx, err := s.beginWriteLocked(context.Background())
	if err != nil {
		return err
	}
	if id != "" {
		if _, err := s.getVisibleEntryTx(context.Background(), tx, id); err != nil {
			_ = tx.Rollback()
			return fmt.Errorf("set leaf %q: %w", id, err)
		}
	}
	if err := setLeafTx(context.Background(), tx, id); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit leaf update", err)
	}
	s.leaf = id
	return nil
}

// ResumeSession validates an existing entry and makes it the current leaf.
func (s *SQLiteStore) ResumeSession(ctx context.Context, entryID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return err
	}
	if _, err := s.getVisibleEntryTx(ctx, tx, entryID); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := setLeafTx(ctx, tx, entryID); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit session resume", err)
	}
	s.leaf = entryID
	return nil
}

func (s *SQLiteStore) GetInputs(ctx context.Context, workdir string, n int) ([]string, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return nil, err
	}
	rows, err := s.db.QueryContext(ctx,
		"SELECT input FROM input_history WHERE workdir = ? ORDER BY timestamp DESC, id DESC LIMIT ?",
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
	if input == "" {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx,
		"INSERT INTO input_history (workdir, input, timestamp) VALUES (?, ?, ?)",
		workdir, input, time.Now().Unix()); err != nil {
		return classifySQLiteError("insert input history", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit input history", err)
	}
	return nil
}

func (s *SQLiteStore) ListSessions(ctx context.Context, workdir string) ([]SessionInfoEntry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return nil, err
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
	if info.ID() == "" {
		return fmt.Errorf("session id is required")
	}
	updatedAt := info.UpdatedAt
	if updatedAt.IsZero() {
		updatedAt = time.Now()
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO sessions (session_id, workdir, model, branch, name, summary, last_preview, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(session_id) DO UPDATE SET
			workdir = excluded.workdir,
			model = excluded.model,
			branch = excluded.branch,
			name = excluded.name,
			summary = excluded.summary,
			last_preview = excluded.last_preview,
			updated_at = excluded.updated_at`,
		info.ID(), info.Workdir, info.Model, info.Branch, info.Name, info.Summary, info.LastPreview, updatedAt.Unix()); err != nil {
		return classifySQLiteError("update session catalog", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit session catalog", err)
	}
	return nil
}

// GetSessionInfo returns one catalog record by its session/leaf ID.
// It is a concrete catalog lookup for transport boundaries; Store intentionally
// remains focused on the active tree and does not grow a catalog interface.
func (s *SQLiteStore) GetSessionInfo(ctx context.Context, id string) (SessionInfoEntry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return SessionInfoEntry{}, err
	}
	var info SessionInfoEntry
	var updatedAt int64
	err := s.db.QueryRowContext(ctx,
		"SELECT session_id, workdir, model, branch, name, summary, last_preview, updated_at FROM sessions WHERE session_id = ?",
		id,
	).Scan(
		&info.EntryBase.ID,
		&info.Workdir,
		&info.Model,
		&info.Branch,
		&info.Name,
		&info.Summary,
		&info.LastPreview,
		&updatedAt,
	)
	if err != nil {
		return SessionInfoEntry{}, err
	}
	info.EntryBase.Timestamp = time.Unix(updatedAt, 0)
	info.UpdatedAt = info.EntryBase.Timestamp
	return info, nil
}

// Append persists an entry to the store.
func (s *SQLiteStore) Append(ctx context.Context, entry Entry) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return "", err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return "", err
	}
	id, err := s.appendTx(ctx, tx, "", entry)
	if err != nil {
		_ = tx.Rollback()
		return "", err
	}
	if err := tx.Commit(); err != nil {
		return "", classifySQLiteError("commit entry", err)
	}
	return id, nil
}

// appendTx is the internal transaction-bound entry writer. A non-empty turn
// ID makes the entry invisible to normal replay until that turn commits.
func (s *SQLiteStore) appendTx(ctx context.Context, tx *sql.Tx, turnID string, entry Entry) (string, error) {
	if entry == nil || entry.ID() == "" {
		return "", fmt.Errorf("entry ID is required")
	}
	id := entry.ID()
	parentID := entry.ParentID()
	ts := entry.When().UnixMilli()
	typ, payload, err := encodeEntry(entry)
	if err != nil {
		return "", err
	}
	sequence, err := nextSequenceTx(ctx, tx)
	if err != nil {
		return "", err
	}
	_, err = tx.ExecContext(ctx,
		"INSERT INTO entries(id,parent_id,type,timestamp,sequence,turn_id,payload) VALUES(?,?,?,?,?,?,?)",
		id, parentID, typ, ts, sequence, nullableString(turnID), payload,
	)
	if err != nil {
		return "", classifySQLiteError("insert entry", err)
	}
	return id, nil
}

// AppendLeafEntry appends an entry and atomically updates the leaf pointer.
// Both operations happen under one lock so concurrent AppendLeaf calls cannot
// interleave GetLeafID → Append → SetLeafID.
func (s *SQLiteStore) AppendLeafEntry(ctx context.Context, entry Entry) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return "", err
	}
	if entry == nil || entry.ID() == "" {
		return "", fmt.Errorf("entry ID is required")
	}
	if entry.ParentID() != s.leaf {
		return "", fmt.Errorf("entry %q parent %q does not match current leaf %q", entry.ID(), entry.ParentID(), s.leaf)
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return "", err
	}
	id, err := s.appendTx(ctx, tx, "", entry)
	if err != nil {
		_ = tx.Rollback()
		return "", err
	}
	if err := setLeafTx(ctx, tx, id); err != nil {
		_ = tx.Rollback()
		return "", err
	}
	if err := tx.Commit(); err != nil {
		return "", classifySQLiteError("commit leaf entry", err)
	}
	s.leaf = id
	return id, nil
}

// AppendBatch persists multiple entries atomically using a SQLite transaction.
func (s *SQLiteStore) AppendBatch(ctx context.Context, entries []Entry) ([]string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return nil, err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()

	ids := make([]string, len(entries))
	for i, entry := range entries {
		id, err := s.appendTx(ctx, tx, "", entry)
		if err != nil {
			return nil, err
		}
		ids[i] = id
	}

	if err := tx.Commit(); err != nil {
		return nil, classifySQLiteError("commit entry batch", err)
	}
	return ids, nil
}

// MoveTo records navigation, changes the selected leaf, and optionally adds a
// summary in one transaction. The selected branch is never partially changed.
func (s *SQLiteStore) MoveTo(ctx context.Context, entryID string, summary *BranchSummaryData) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return "", err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return "", err
	}
	if _, err := s.getVisibleEntryTx(ctx, tx, entryID); err != nil {
		_ = tx.Rollback()
		return "", fmt.Errorf("move to %q: %w", entryID, err)
	}
	oldLeaf := s.leaf
	leafEntry := &LeafEntry{
		EntryBase: EntryBase{ID: newID(), ParentID: oldLeaf, Timestamp: time.Now()},
		TargetID:  entryID,
	}
	if _, err := s.appendTx(ctx, tx, "", leafEntry); err != nil {
		_ = tx.Rollback()
		return "", fmt.Errorf("record leaf move: %w", err)
	}
	finalLeaf := entryID
	var summaryID string
	if summary != nil {
		fromID := summary.FromID
		if fromID == "" {
			fromID = oldLeaf
		}
		summaryEntry := &BranchSummaryEntry{
			EntryBase: EntryBase{ID: newID(), ParentID: entryID, Timestamp: time.Now()},
			FromID:    fromID,
			Summary:   summary.Summary,
			Details:   append([]byte(nil), summary.Details...),
		}
		summaryID, err = s.appendTx(ctx, tx, "", summaryEntry)
		if err != nil {
			_ = tx.Rollback()
			return "", fmt.Errorf("record branch summary: %w", err)
		}
		finalLeaf = summaryID
	}
	if err := setLeafTx(ctx, tx, finalLeaf); err != nil {
		_ = tx.Rollback()
		return "", err
	}
	if err := tx.Commit(); err != nil {
		return "", classifySQLiteError("commit tree navigation", err)
	}
	s.leaf = finalLeaf
	return summaryID, nil
}

// GetEntry returns a single entry by ID.
func (s *SQLiteStore) GetEntry(ctx context.Context, id string) (Entry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.closed || s.db == nil {
		return nil, ErrSessionClosed
	}
	return s.getEntry(ctx, id)
}

// getEntry is the internal (unlocked) entry reader.
func (s *SQLiteStore) getEntry(ctx context.Context, id string) (Entry, error) {
	row := s.db.QueryRowContext(ctx, "SELECT id,parent_id,type,timestamp,payload FROM entries WHERE id=?", id)
	return scanEntry(row)
}

// Branch returns entries from root to the current leaf.
func (s *SQLiteStore) Branch(ctx context.Context) ([]Entry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.closed || s.db == nil {
		return nil, ErrSessionClosed
	}
	return s.branchAtLocked(ctx, s.leaf)
}

// BranchAt returns entries from root to the explicitly supplied leaf. The
// caller owns the leaf identity; this method never consults the mutable store
// leaf after entry, which makes it safe for snapshot construction.
func (s *SQLiteStore) BranchAt(ctx context.Context, leafID string) ([]Entry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.closed || s.db == nil {
		return nil, ErrSessionClosed
	}
	return s.branchAtLocked(ctx, leafID)
}

func (s *SQLiteStore) branchAtLocked(ctx context.Context, leafID string) ([]Entry, error) {
	if leafID == "" {
		return nil, nil
	}

	// Reconstruct the parent chain and decode its rows in one query. The path
	// guard keeps malformed cycles from spinning forever; valid entry IDs are
	// generated hex strings, so slash-delimited membership is unambiguous.
	rows, err := s.db.QueryContext(ctx, `
		WITH RECURSIVE branch(id, parent_id, depth, path) AS (
			SELECT e.id, e.parent_id, 0, '/' || e.id || '/'
			FROM entries e
			WHERE e.id = ?
			  AND (e.turn_id IS NULL OR EXISTS (
				SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
			  ))
			UNION ALL
			SELECT e.id, e.parent_id, branch.depth + 1, branch.path || e.id || '/'
			FROM entries e
			JOIN branch ON e.id = branch.parent_id
			WHERE (e.turn_id IS NULL OR EXISTS (
				SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
			))
			  AND instr(branch.path, '/' || e.id || '/') = 0
		)
		SELECT e.id, e.parent_id, e.type, e.timestamp, e.payload
		FROM branch
		JOIN entries e ON e.id = branch.id
		ORDER BY branch.depth DESC
	`, leafID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	entries := make([]Entry, 0)
	for rows.Next() {
		entry, err := scanEntry(rows)
		if err != nil {
			return nil, err
		}
		entries = append(entries, entry)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	if len(entries) == 0 {
		return nil, sql.ErrNoRows
	}
	return entries, nil
}

// Entries returns all entries in the session.
func (s *SQLiteStore) Entries(ctx context.Context) ([]Entry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.closed || s.db == nil {
		return nil, ErrSessionClosed
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT e.id,e.parent_id,e.type,e.timestamp,e.payload
		FROM entries e
		WHERE e.turn_id IS NULL OR EXISTS (
			SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
		)
		ORDER BY e.sequence ASC, e.rowid ASC`)
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
	FromID  string `json:"from_id,omitempty"`
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
		p.FromID = e.FromID
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
	return decodeEntry(EntryBase{ID: id, ParentID: parentID, Timestamp: time.UnixMilli(ts)}, typ, payload)
}

func decodeEntry(base EntryBase, typ string, payload []byte) (Entry, error) {
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
		return &BranchSummaryEntry{EntryBase: base, FromID: p.FromID, Summary: p.Summary, Details: p.Details}, nil
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
