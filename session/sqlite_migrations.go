package session

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"os"
	"sort"
	"strings"
	"time"

	"golang.org/x/sys/unix"
)

// SQLite schema migration and file-lock lifecycle.
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
		return rollback(
			fmt.Errorf("%w: found %d, supported through %d", ErrUnsupportedSchema, version, currentSchemaVersion),
		)
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
	if err := ensureColumn(ctx, tx, "turns", "input_images", "BLOB NOT NULL DEFAULT '[]'"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "turns", "context_id", "TEXT NOT NULL DEFAULT ''"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "actions", "session_id", "TEXT NOT NULL DEFAULT ''"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "actions", "turn_id", "TEXT NOT NULL DEFAULT ''"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "sessions", "leaf_id", "TEXT NOT NULL DEFAULT ''"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "actions", "metadata", "BLOB NOT NULL DEFAULT '{}'"); err != nil {
		return rollback(err)
	}
	if err := ensureColumn(ctx, tx, "actions", "preimages", "BLOB NOT NULL DEFAULT '[]'"); err != nil {
		return rollback(err)
	}
	if err := ensureActionProcessIdentity(ctx, tx); err != nil {
		return rollback(err)
	}
	if _, err := tx.ExecContext(ctx, `
		CREATE INDEX IF NOT EXISTS idx_entries_parent ON entries(parent_id);
		CREATE INDEX IF NOT EXISTS idx_entries_type ON entries(type);
		CREATE INDEX IF NOT EXISTS idx_entries_sequence ON entries(sequence);
		CREATE INDEX IF NOT EXISTS idx_entries_turn ON entries(turn_id);
		CREATE INDEX IF NOT EXISTS idx_turns_state ON turns(state, sequence);
		CREATE INDEX IF NOT EXISTS idx_actions_state ON actions(state, prepared_at);
		CREATE INDEX IF NOT EXISTS idx_actions_fingerprint ON actions(fingerprint);
		CREATE INDEX IF NOT EXISTS idx_action_transitions_action ON action_transitions(action_id, id);
		CREATE INDEX IF NOT EXISTS idx_sessions_leaf ON sessions(leaf_id);
	`); err != nil {
		return rollback(fmt.Errorf("create session indexes: %w", err))
	}
	if _, err := tx.ExecContext(ctx, "UPDATE entries SET sequence = rowid WHERE sequence = 0"); err != nil {
		return rollback(fmt.Errorf("backfill entry sequence: %w", err))
	}
	if _, err := tx.ExecContext(ctx, "UPDATE sessions SET leaf_id = session_id WHERE leaf_id = ''"); err != nil {
		return rollback(fmt.Errorf("backfill session catalog leaves: %w", err))
	}
	if version < currentSchemaVersion {
		if err := consolidateLegacySessionCatalog(ctx, tx); err != nil {
			return rollback(err)
		}
	}
	if _, err := tx.ExecContext(ctx, fmt.Sprintf("PRAGMA user_version = %d", currentSchemaVersion)); err != nil {
		return rollback(fmt.Errorf("write schema version: %w", err))
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit schema migration", err)
	}
	return nil
}

// ensureActionProcessIdentity performs the one intentional rename from the
// pre-identity action schema. The stored value is opaque; preserving it keeps
// interrupted actions recoverable while removing the misleading domain name.
type legacySessionCatalogRow struct {
	id        string
	leafID    string
	updatedAt int64
	sequence  int64
	timestamp int64
}

// consolidateLegacySessionCatalog collapses the pre-v11 catalog, which used
// every mutable leaf as a session key, into one row per tree root. The
// active leaf wins when available; otherwise the highest durable entry
// sequence supplies the selected leaf and metadata. Each migrated row gets a
// fresh identity so historical leaf IDs remain unambiguous lookup keys.
func consolidateLegacySessionCatalog(ctx context.Context, tx *sql.Tx) error {
	rows, err := tx.QueryContext(ctx, "SELECT session_id, leaf_id, updated_at FROM sessions")
	if err != nil {
		return fmt.Errorf("read legacy session catalog: %w", err)
	}
	defer rows.Close()
	var catalogRows []legacySessionCatalogRow
	for rows.Next() {
		var row legacySessionCatalogRow
		if err := rows.Scan(&row.id, &row.leafID, &row.updatedAt); err != nil {
			return fmt.Errorf("scan legacy session catalog: %w", err)
		}
		if row.leafID == "" {
			row.leafID = row.id
		}
		catalogRows = append(catalogRows, row)
	}
	if err := rows.Err(); err != nil {
		return fmt.Errorf("read legacy session catalog rows: %w", err)
	}
	if len(catalogRows) == 0 {
		return nil
	}

	parents := make(map[string]string)
	entryMetadata := make(map[string]struct {
		sequence  int64
		timestamp int64
	})
	entryRows, err := tx.QueryContext(ctx, "SELECT id, parent_id, sequence, timestamp FROM entries")
	if err != nil {
		return fmt.Errorf("read session entry ancestry: %w", err)
	}
	for entryRows.Next() {
		var id, parentID string
		var sequence, timestamp int64
		if err := entryRows.Scan(&id, &parentID, &sequence, &timestamp); err != nil {
			entryRows.Close()
			return fmt.Errorf("scan session entry ancestry: %w", err)
		}
		parents[id] = parentID
		entryMetadata[id] = struct {
			sequence  int64
			timestamp int64
		}{sequence: sequence, timestamp: timestamp}
	}
	if err := entryRows.Err(); err != nil {
		entryRows.Close()
		return fmt.Errorf("read session entry ancestry rows: %w", err)
	}
	if err := entryRows.Close(); err != nil {
		return fmt.Errorf("close session entry ancestry: %w", err)
	}
	var activeLeaf sql.NullString
	if err := tx.QueryRowContext(ctx, "SELECT value FROM session_meta WHERE key = 'leaf_id'").
		Scan(&activeLeaf); err != nil &&
		err != sql.ErrNoRows {
		return fmt.Errorf("read active session leaf: %w", err)
	}
	for i := range catalogRows {
		if metadata, ok := entryMetadata[catalogRows[i].leafID]; ok {
			catalogRows[i].sequence = metadata.sequence
			catalogRows[i].timestamp = metadata.timestamp
		}
	}

	rootFor := func(leafID string) string {
		seen := make(map[string]struct{})
		current := leafID
		for current != "" {
			if _, exists := seen[current]; exists {
				return leafID
			}
			seen[current] = struct{}{}
			parentID, exists := parents[current]
			if !exists || parentID == "" {
				return current
			}
			current = parentID
		}
		return leafID
	}

	groups := make(map[string][]legacySessionCatalogRow)
	for _, row := range catalogRows {
		rootID := rootFor(row.leafID)
		groups[rootID] = append(groups[rootID], row)
	}
	for _, group := range groups {
		sort.SliceStable(group, func(i, j int) bool {
			iActive := activeLeaf.Valid && group[i].leafID == activeLeaf.String
			jActive := activeLeaf.Valid && group[j].leafID == activeLeaf.String
			if iActive != jActive {
				return iActive
			}
			if group[i].sequence != group[j].sequence {
				return group[i].sequence > group[j].sequence
			}
			if group[i].updatedAt != group[j].updatedAt {
				return group[i].updatedAt > group[j].updatedAt
			}
			if group[i].timestamp != group[j].timestamp {
				return group[i].timestamp > group[j].timestamp
			}
			return group[i].id > group[j].id
		})
		winner := group[0]
		for _, row := range group[1:] {
			if _, err := tx.ExecContext(ctx, "DELETE FROM sessions WHERE session_id = ?", row.id); err != nil {
				return fmt.Errorf("remove superseded session catalog row %q: %w", row.id, err)
			}
		}
		if _, err := tx.ExecContext(
			ctx,
			"UPDATE sessions SET session_id = ?, leaf_id = ? WHERE session_id = ?",
			newID(),
			winner.leafID,
			winner.id,
		); err != nil {
			return fmt.Errorf("assign migrated session catalog identity for leaf %q: %w", winner.leafID, err)
		}
	}
	return nil
}

func ensureActionProcessIdentity(ctx context.Context, tx *sql.Tx) error {
	rows, err := tx.QueryContext(ctx, "PRAGMA table_info(actions)")
	if err != nil {
		return fmt.Errorf("inspect actions schema: %w", err)
	}
	defer rows.Close()
	var hasIdentity, hasGroup bool
	for rows.Next() {
		var cid, notNull, pk int
		var name, typ string
		var defaultValue sql.NullString
		if err := rows.Scan(&cid, &name, &typ, &notNull, &defaultValue, &pk); err != nil {
			return fmt.Errorf("inspect actions column: %w", err)
		}
		switch name {
		case "process_identity":
			hasIdentity = true
		case "process_group_id":
			hasGroup = true
		}
	}
	if err := rows.Err(); err != nil {
		return fmt.Errorf("inspect actions columns: %w", err)
	}
	if hasIdentity && hasGroup {
		var conflicts int
		if err := tx.QueryRowContext(ctx, `
			SELECT count(*) FROM actions
			WHERE process_identity <> '' AND process_group_id <> ''
			  AND process_identity <> process_group_id`).Scan(&conflicts); err != nil {
			return fmt.Errorf("inspect dual process identity columns: %w", err)
		}
		if conflicts != 0 {
			return fmt.Errorf("%w: conflicting process identity columns", ErrCorruptSession)
		}
		if _, err := tx.ExecContext(ctx, `
			UPDATE actions SET process_identity = process_group_id
			WHERE process_identity = '' AND process_group_id <> ''`); err != nil {
			return fmt.Errorf("merge legacy process identity column: %w", err)
		}
		if _, err := tx.ExecContext(ctx, "ALTER TABLE actions DROP COLUMN process_group_id"); err != nil {
			return fmt.Errorf("drop legacy process identity column: %w", err)
		}
		return nil
	}
	if hasIdentity {
		return nil
	}
	if hasGroup {
		if _, err := tx.ExecContext(
			ctx,
			"ALTER TABLE actions RENAME COLUMN process_group_id TO process_identity",
		); err != nil {
			return fmt.Errorf("rename action process identity column: %w", err)
		}
		return nil
	}
	return ensureColumn(ctx, tx, "actions", "process_identity", "TEXT NOT NULL DEFAULT ''")
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
			input_images BLOB NOT NULL DEFAULT '[]',
			context_id TEXT NOT NULL DEFAULT '',
			started_at INTEGER NOT NULL,
			ended_at INTEGER NOT NULL DEFAULT 0,
			error TEXT NOT NULL DEFAULT ''
		)`,
		`CREATE TABLE IF NOT EXISTS actions (
			action_id TEXT PRIMARY KEY,
			invocation_id TEXT NOT NULL,
			session_id TEXT NOT NULL DEFAULT '',
			turn_id TEXT NOT NULL DEFAULT '',
			tool_name TEXT NOT NULL,
			category TEXT NOT NULL DEFAULT '',
			operation TEXT NOT NULL,
			arguments BLOB NOT NULL,
			metadata BLOB NOT NULL DEFAULT '{}',
			preimages BLOB NOT NULL DEFAULT '[]',
			fingerprint TEXT NOT NULL,
			cwd TEXT NOT NULL DEFAULT '',
			paths BLOB NOT NULL DEFAULT '[]',
			environment BLOB NOT NULL DEFAULT '[]',
			network_intent TEXT NOT NULL DEFAULT '',
			mcp_identity TEXT NOT NULL DEFAULT '',
			policy_mode TEXT NOT NULL DEFAULT '',
			state TEXT NOT NULL,
			authorization TEXT NOT NULL DEFAULT '',
			result_identity TEXT NOT NULL DEFAULT '',
			error TEXT NOT NULL DEFAULT '',
			cleanup_outcome TEXT NOT NULL DEFAULT '',
			process_identity TEXT NOT NULL DEFAULT '',
			prepared_at INTEGER NOT NULL,
			authorized_at INTEGER NOT NULL DEFAULT 0,
			started_at INTEGER NOT NULL DEFAULT 0,
			ended_at INTEGER NOT NULL DEFAULT 0
		)`,
		`CREATE TABLE IF NOT EXISTS action_transitions (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			action_id TEXT NOT NULL,
			from_state TEXT NOT NULL DEFAULT '',
			to_state TEXT NOT NULL,
			reason TEXT NOT NULL DEFAULT '',
			timestamp INTEGER NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS input_history (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			workdir TEXT NOT NULL,
			input TEXT NOT NULL,
			timestamp INTEGER NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS sessions (
			session_id TEXT PRIMARY KEY,
			leaf_id TEXT NOT NULL DEFAULT '',
			workdir TEXT NOT NULL,
			model TEXT NOT NULL DEFAULT '',
			branch TEXT NOT NULL DEFAULT '',
			name TEXT NOT NULL DEFAULT '',
			summary TEXT NOT NULL DEFAULT '',
			last_preview TEXT NOT NULL DEFAULT '',
			updated_at INTEGER NOT NULL DEFAULT 0
		)`,
		`CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
			entry_id UNINDEXED,
			role,
			content,
			tokenize = 'porter unicode61'
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
