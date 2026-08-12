package session

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"

	_ "modernc.org/sqlite"
)

// SQLiteStore implements Store backed by SQLite. One row per entry,
// discriminated by type; message payload as JSON content blocks.
// Tree structure via parent_id; leaf pointer via session_meta table.
type SQLiteStore struct {
	db              *sql.DB
	mu              sync.RWMutex
	meta            Metadata
	leaf            string
	activationOwner uint64
	closed          bool
	closeErr        error
	closeOnce       sync.Once
	lockFile        *os.File
}

var _ Store = (*SQLiteStore)(nil)

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
	if err := recoverInterruptedActions(context.Background(), db); err != nil {
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
	if _, err := tx.ExecContext(
		ctx,
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
	if _, err := tx.ExecContext(
		ctx,
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
	if err := s.db.QueryRow("SELECT value FROM session_meta WHERE key='session_id'").
		Scan(&storedID); err != nil &&
		err != sql.ErrNoRows {
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
	if err := s.db.QueryRow("SELECT value FROM session_meta WHERE key='leaf_id'").
		Scan(&leaf); err != nil &&
		err != sql.ErrNoRows {
		return err
	}
	if leaf.Valid {
		s.leaf = leaf.String
	}
	// Load session name.
	var name sql.NullString
	if err := s.db.QueryRow("SELECT value FROM session_meta WHERE key='name'").
		Scan(&name); err != nil &&
		err != sql.ErrNoRows {
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
	if _, err := s.branchAtLocked(context.Background(), s.leaf); err != nil {
		return fmt.Errorf("%w: validate session branch from leaf %q: %v", ErrCorruptSession, s.leaf, err)
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

// ActivationOwner identifies the latest host-owned provisional or committed
// selection in this store process. It is an in-memory CAS token for runtime
// replacement rollback; durable session identity and leaf remain in SQLite.
func (s *SQLiteStore) ActivationOwner() uint64 {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.activationOwner
}

func (s *SQLiteStore) nextActivationOwnerLocked() uint64 {
	s.activationOwner++
	if s.activationOwner == 0 {
		s.activationOwner++
	}
	return s.activationOwner
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

// ActivateSession atomically selects a logical catalog identity and leaf.
// An empty leaf selects the virtual root. The identity and leaf are committed
// together so a failed resume cannot leave either half of the runtime
// selection installed.
func (s *SQLiteStore) ActivateSession(ctx context.Context, identity, leafID string) error {
	identity = strings.TrimSpace(identity)
	if identity == "" {
		return errors.New("session identity is required")
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
	if leafID != "" {
		if _, err := s.getVisibleEntryTx(ctx, tx, leafID); err != nil {
			_ = tx.Rollback()
			return fmt.Errorf("activate leaf %q: %w", leafID, err)
		}
	}
	if _, err := tx.ExecContext(
		ctx,
		"INSERT INTO session_meta(key,value) VALUES('session_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		identity,
	); err != nil {
		_ = tx.Rollback()
		return fmt.Errorf("persist session identity: %w", err)
	}
	if err := setLeafTx(ctx, tx, leafID); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit session activation", err)
	}
	s.meta.ID = identity
	s.leaf = leafID
	s.nextActivationOwnerLocked()
	return nil
}

// RestoreSessionIfOwner restores the selection captured before a provisional
// activation, but only while that activation still owns the store. A newer
// replacement therefore cannot be overwritten by a stale close path.
func (s *SQLiteStore) RestoreSessionIfOwner(
	ctx context.Context,
	owner uint64,
	identity, leafID string,
) error {
	if owner == 0 || strings.TrimSpace(identity) == "" {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return err
	}
	if s.activationOwner != owner {
		return nil
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return err
	}
	if _, err := tx.ExecContext(
		ctx,
		"INSERT INTO session_meta(key,value) VALUES('session_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		identity,
	); err != nil {
		_ = tx.Rollback()
		return fmt.Errorf("restore session identity: %w", err)
	}
	if err := setLeafTx(ctx, tx, leafID); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit session restoration", err)
	}
	s.meta.ID = identity
	s.leaf = leafID
	s.nextActivationOwnerLocked()
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
	resolvedLeaf := entryID
	resolvedIdentity := entryID
	var catalogLeaf, catalogIdentity string
	lookupErr := tx.QueryRowContext(
		ctx,
		`SELECT session_id, leaf_id FROM (`+sessionCatalogLookupSQL+`)`,
		entryID,
		entryID,
		entryID,
		entryID,
	).Scan(&catalogIdentity, &catalogLeaf)
	if lookupErr == nil {
		resolvedIdentity = catalogIdentity
		if entryID == catalogIdentity && catalogLeaf != "" {
			resolvedLeaf = catalogLeaf
		}
	} else if lookupErr != sql.ErrNoRows {
		_ = tx.Rollback()
		return fmt.Errorf("look up session resume %q: %w", entryID, lookupErr)
	} else {
		if entryID == s.meta.ID && s.leaf != "" {
			// A stable runtime identity can be resumed before its first catalog
			// publication; the store's durable leaf is its checkpoint.
			resolvedLeaf = s.leaf
		} else {
			// A visible leaf created before catalog publication starts a new
			// logical session identity; the requested checkpoint remains exact.
			resolvedIdentity = newID()
		}
	}
	if _, err := s.getVisibleEntryTx(ctx, tx, resolvedLeaf); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := setLeafTx(ctx, tx, resolvedLeaf); err != nil {
		_ = tx.Rollback()
		return err
	}
	if _, err := tx.ExecContext(
		ctx,
		"INSERT INTO session_meta(key,value) VALUES('session_id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		resolvedIdentity,
	); err != nil {
		_ = tx.Rollback()
		return fmt.Errorf("persist resumed session identity: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit session resume", err)
	}
	s.meta.ID = resolvedIdentity
	s.leaf = resolvedLeaf
	s.nextActivationOwnerLocked()
	return nil
}
