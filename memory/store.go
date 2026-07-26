// Package memory provides explicit, workspace-scoped notes for Ion.
//
// Memory is deliberately separate from session persistence. It is queried by
// explicit tools or host commands; it is never injected into a prompt
// implicitly and its records never become session-tree entries.
package memory

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	_ "modernc.org/sqlite"
)

const (
	MaxContentBytes = 32 * 1024
	MaxTagsBytes    = 1024
	MaxQueryBytes   = 1024
	DefaultLimit    = 20
	MaxLimit        = 100
)

var (
	ErrNotFound       = errors.New("memory record not found")
	ErrAlreadyActive  = errors.New("memory record is already active")
	ErrAlreadyDeleted = errors.New("memory record is already deleted")
)

const schema = `
CREATE TABLE IF NOT EXISTS memories (
    id         TEXT PRIMARY KEY,
    scope      TEXT NOT NULL,
    content    TEXT NOT NULL,
    tags       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    deleted_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_memories_scope_active
    ON memories(scope, deleted_at, created_at DESC);
CREATE TABLE IF NOT EXISTS memory_audit (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id  TEXT NOT NULL,
    scope      TEXT NOT NULL,
    operation  TEXT NOT NULL,
    content    TEXT NOT NULL,
    tags       TEXT NOT NULL DEFAULT '',
    at         INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_audit_scope
    ON memory_audit(scope, at DESC);
`

// Record is one user- or model-authored memory note.
type Record struct {
	ID        string
	Scope     string
	Content   string
	Tags      string
	CreatedAt time.Time
	DeletedAt *time.Time
}

// AuditEntry records a state-changing memory operation.
type AuditEntry struct {
	Sequence  int64
	MemoryID  string
	Scope     string
	Operation string
	Content   string
	Tags      string
	At        time.Time
}

// Store is a SQLite-backed memory store. A single connection keeps writes and
// close behavior deterministic when the TUI and runtime tools share a file.
type Store struct {
	db     *sql.DB
	mu     sync.RWMutex
	closed bool
}

// Open opens or creates a memory database at path. If path names a directory,
// memory.db is created inside it.
func Open(path string) (*Store, error) {
	path = strings.TrimSpace(path)
	if path == "" {
		return nil, fmt.Errorf("memory path is required")
	}
	if path != ":memory:" {
		info, err := os.Stat(path)
		switch {
		case err == nil && info.IsDir():
			path = filepath.Join(path, "memory.db")
		case err != nil && !errors.Is(err, os.ErrNotExist):
			return nil, fmt.Errorf("stat memory path: %w", err)
		}
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			return nil, fmt.Errorf("create memory directory: %w", err)
		}
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, fmt.Errorf("open memory database: %w", err)
	}
	db.SetMaxOpenConns(1)
	if _, err := db.Exec(schema); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize memory database: %w", err)
	}
	return &Store{db: db}, nil
}

// Add appends an active memory record and its audit entry.
func (s *Store) Add(ctx context.Context, scope, content, tags string) (Record, error) {
	scope, content, tags, err := validateInput(scope, content, tags)
	if err != nil {
		return Record{}, err
	}
	id, err := newID()
	if err != nil {
		return Record{}, err
	}
	now := time.Now().UTC()
	s.mu.RLock()
	defer s.mu.RUnlock()
	tx, err := s.begin(ctx)
	if err != nil {
		return Record{}, err
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(
		ctx,
		`INSERT INTO memories(id, scope, content, tags, created_at) VALUES(?, ?, ?, ?, ?)`,
		id, scope, content, tags, now.UnixNano(),
	); err != nil {
		return Record{}, fmt.Errorf("insert memory: %w", err)
	}
	if err := insertAudit(ctx, tx, id, scope, "add", content, tags, now); err != nil {
		return Record{}, err
	}
	if err := tx.Commit(); err != nil {
		return Record{}, fmt.Errorf("commit memory: %w", err)
	}
	return Record{ID: id, Scope: scope, Content: content, Tags: tags, CreatedAt: now}, nil
}

// Search returns active records matching query, newest first. An empty query
// lists recent records. Matching is intentionally literal and bounded; memory
// is a small explicit note store, not an implicit semantic index.
func (s *Store) Search(ctx context.Context, scope, query string, limit int) ([]Record, error) {
	scope, err := normalizeScope(scope)
	if err != nil {
		return nil, err
	}
	query = strings.TrimSpace(query)
	if len([]byte(query)) > MaxQueryBytes {
		return nil, fmt.Errorf("memory query exceeds %d bytes", MaxQueryBytes)
	}
	limit = normalizeLimit(limit)
	s.mu.RLock()
	defer s.mu.RUnlock()
	rows, err := s.query(ctx, `
		SELECT id, scope, content, tags, created_at, deleted_at
		FROM memories
		WHERE scope = ? AND deleted_at = 0
		  AND (? = '' OR instr(lower(content || ' ' || tags), lower(?)) > 0)
		ORDER BY created_at DESC
		LIMIT ?`, scope, query, query, limit)
	if err != nil {
		return nil, err
	}
	return rows, nil
}

// List returns records in a scope. Deleted records are included only when
// includeDeleted is true.
func (s *Store) List(ctx context.Context, scope string, includeDeleted bool, limit int) ([]Record, error) {
	scope, err := normalizeScope(scope)
	if err != nil {
		return nil, err
	}
	limit = normalizeLimit(limit)
	where := "deleted_at = 0"
	if includeDeleted {
		where = "1 = 1"
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	rows, err := s.query(ctx, `SELECT id, scope, content, tags, created_at, deleted_at
		FROM memories WHERE scope = ? AND `+where+` ORDER BY created_at DESC LIMIT ?`, scope, limit)
	if err != nil {
		return nil, err
	}
	return rows, nil
}

// Delete soft-deletes a record and appends an audit entry. It never removes
// content, so the host can offer an explicit restore operation.
func (s *Store) Delete(ctx context.Context, scope, id string) (Record, error) {
	return s.transition(ctx, scope, id, true)
}

// Restore reactivates a soft-deleted record and appends an audit entry.
func (s *Store) Restore(ctx context.Context, scope, id string) (Record, error) {
	return s.transition(ctx, scope, id, false)
}

// Audit returns recent state-changing operations for one scope.
func (s *Store) Audit(ctx context.Context, scope string, limit int) ([]AuditEntry, error) {
	scope, err := normalizeScope(scope)
	if err != nil {
		return nil, err
	}
	limit = normalizeLimit(limit)
	s.mu.RLock()
	defer s.mu.RUnlock()
	rows, err := s.queryAudit(ctx, scope, limit)
	if err != nil {
		return nil, err
	}
	return rows, nil
}

func (s *Store) transition(ctx context.Context, scope, id string, deleted bool) (Record, error) {
	scope, err := normalizeScope(scope)
	if err != nil {
		return Record{}, err
	}
	id = strings.TrimSpace(id)
	if id == "" {
		return Record{}, fmt.Errorf("memory id is required")
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	tx, err := s.begin(ctx)
	if err != nil {
		return Record{}, err
	}
	defer tx.Rollback()
	record, err := scanRecord(tx.QueryRowContext(ctx,
		`SELECT id, scope, content, tags, created_at, deleted_at FROM memories WHERE id = ? AND scope = ?`, id, scope))
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return Record{}, ErrNotFound
		}
		return Record{}, err
	}
	if (record.DeletedAt != nil) == deleted {
		if deleted {
			return Record{}, ErrAlreadyDeleted
		}
		return Record{}, ErrAlreadyActive
	}
	now := time.Now().UTC()
	deletedAt := int64(0)
	if deleted {
		deletedAt = now.UnixNano()
	}
	if _, err := tx.ExecContext(
		ctx,
		`UPDATE memories SET deleted_at = ? WHERE id = ? AND scope = ?`,
		deletedAt,
		id,
		scope,
	); err != nil {
		return Record{}, fmt.Errorf("update memory: %w", err)
	}
	op := "restore"
	if deleted {
		op = "delete"
	}
	if err := insertAudit(ctx, tx, id, scope, op, record.Content, record.Tags, now); err != nil {
		return Record{}, err
	}
	if err := tx.Commit(); err != nil {
		return Record{}, fmt.Errorf("commit memory transition: %w", err)
	}
	if deleted {
		record.DeletedAt = &now
	} else {
		record.DeletedAt = nil
	}
	return record, nil
}

func (s *Store) query(ctx context.Context, query string, args ...any) ([]Record, error) {
	if err := s.checkOpenLocked(); err != nil {
		return nil, err
	}
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, fmt.Errorf("query memory: %w", err)
	}
	defer rows.Close()
	result := make([]Record, 0)
	for rows.Next() {
		record, err := scanRecord(rows)
		if err != nil {
			return nil, err
		}
		result = append(result, record)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("read memory rows: %w", err)
	}
	return result, nil
}

func (s *Store) queryAudit(ctx context.Context, scope string, limit int) ([]AuditEntry, error) {
	if err := s.checkOpenLocked(); err != nil {
		return nil, err
	}
	rows, err := s.db.QueryContext(ctx, `SELECT seq, memory_id, scope, operation, content, tags, at
		FROM memory_audit WHERE scope = ? ORDER BY seq DESC LIMIT ?`, scope, limit)
	if err != nil {
		return nil, fmt.Errorf("query memory audit: %w", err)
	}
	defer rows.Close()
	result := make([]AuditEntry, 0)
	for rows.Next() {
		var entry AuditEntry
		var at int64
		if err := rows.Scan(
			&entry.Sequence,
			&entry.MemoryID,
			&entry.Scope,
			&entry.Operation,
			&entry.Content,
			&entry.Tags,
			&at,
		); err != nil {
			return nil, fmt.Errorf("scan memory audit: %w", err)
		}
		entry.At = time.Unix(0, at).UTC()
		result = append(result, entry)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("read memory audit: %w", err)
	}
	return result, nil
}

func (s *Store) begin(ctx context.Context) (*sql.Tx, error) {
	if err := s.checkOpenLocked(); err != nil {
		return nil, err
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, fmt.Errorf("begin memory transaction: %w", err)
	}
	return tx, nil
}

func (s *Store) checkOpenLocked() error {
	if s == nil || s.db == nil {
		return fmt.Errorf("memory store is not configured")
	}
	if s.closed {
		return fmt.Errorf("memory store is closed")
	}
	return nil
}

// Close releases the database. It is safe to call more than once.
func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil
	}
	s.closed = true
	return s.db.Close()
}

func insertAudit(ctx context.Context, tx *sql.Tx, id, scope, operation, content, tags string, at time.Time) error {
	if _, err := tx.ExecContext(
		ctx,
		`INSERT INTO memory_audit(memory_id, scope, operation, content, tags, at) VALUES(?, ?, ?, ?, ?, ?)`,
		id, scope, operation, content, tags, at.UnixNano(),
	); err != nil {
		return fmt.Errorf("audit memory: %w", err)
	}
	return nil
}

type rowScanner interface {
	Scan(dest ...any) error
}

func scanRecord(row rowScanner) (Record, error) {
	var record Record
	var createdAt, deletedAt int64
	if err := row.Scan(&record.ID, &record.Scope, &record.Content, &record.Tags, &createdAt, &deletedAt); err != nil {
		return Record{}, fmt.Errorf("scan memory: %w", err)
	}
	record.CreatedAt = time.Unix(0, createdAt).UTC()
	if deletedAt != 0 {
		deleted := time.Unix(0, deletedAt).UTC()
		record.DeletedAt = &deleted
	}
	return record, nil
}

func validateInput(scope, content, tags string) (string, string, string, error) {
	scope, err := normalizeScope(scope)
	if err != nil {
		return "", "", "", err
	}
	content = strings.TrimSpace(content)
	if content == "" {
		return "", "", "", fmt.Errorf("memory content is required")
	}
	if len([]byte(content)) > MaxContentBytes || !utf8.ValidString(content) {
		return "", "", "", fmt.Errorf("memory content must be valid UTF-8 and at most %d bytes", MaxContentBytes)
	}
	tags = strings.TrimSpace(tags)
	if len([]byte(tags)) > MaxTagsBytes || !utf8.ValidString(tags) {
		return "", "", "", fmt.Errorf("memory tags must be valid UTF-8 and at most %d bytes", MaxTagsBytes)
	}
	return scope, content, tags, nil
}

func normalizeScope(scope string) (string, error) {
	scope = strings.TrimSpace(scope)
	if scope == "" {
		return "", fmt.Errorf("memory scope is required")
	}
	abs, err := filepath.Abs(scope)
	if err != nil {
		return "", fmt.Errorf("resolve memory scope: %w", err)
	}
	resolved, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return "", fmt.Errorf("resolve memory scope %q: %w", scope, err)
	}
	info, err := os.Stat(resolved)
	if err != nil || !info.IsDir() {
		if err == nil {
			err = fmt.Errorf("not a directory")
		}
		return "", fmt.Errorf("memory scope %q: %w", resolved, err)
	}
	return resolved, nil
}

func normalizeLimit(limit int) int {
	if limit <= 0 {
		return DefaultLimit
	}
	if limit > MaxLimit {
		return MaxLimit
	}
	return limit
}

func newID() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("generate memory id: %w", err)
	}
	return "mem_" + hex.EncodeToString(raw[:]), nil
}
