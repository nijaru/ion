package session

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"
)

// Parent returns the persisted ancestry record for the parent of sessionID.
func (s *SQLiteStore) Parent(ctx context.Context, sessionID string) (*SessionAncestry, error) {
	record, err := s.loadSQLiteAncestry(ctx, sessionID)
	if err != nil {
		return nil, err
	}
	if record.ParentSessionID == "" {
		return nil, nil
	}
	return s.loadSQLiteAncestry(ctx, record.ParentSessionID)
}

// Children lists the persisted ancestry records for direct children of sessionID.
func (s *SQLiteStore) Children(ctx context.Context, sessionID string) ([]SessionAncestry, error) {
	rows, err := s.db.QueryContext(
		ctx,
		`SELECT session_id, parent_session_id, fork_point_event_id, branch_label, fork_reason, depth, created_at
		 FROM session_ancestry
		 WHERE parent_session_id = ?
		 ORDER BY created_at ASC, session_id ASC`,
		sessionID,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var children []SessionAncestry
	for rows.Next() {
		record, err := scanSQLiteAncestry(rows)
		if err != nil {
			return nil, err
		}
		children = append(children, record)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return children, nil
}

// Lineage returns the root-to-current ancestry chain for sessionID.
func (s *SQLiteStore) Lineage(ctx context.Context, sessionID string) ([]SessionAncestry, error) {
	lineage := make([]SessionAncestry, 0, 8)
	seen := make(map[string]struct{}, 8)
	current := sessionID

	for current != "" {
		record, err := s.loadSQLiteAncestry(ctx, current)
		if err != nil {
			return nil, err
		}
		if _, exists := seen[current]; exists {
			return nil, errors.New("session ancestry cycle detected")
		}
		seen[current] = struct{}{}
		lineage = append(lineage, *record)
		current = record.ParentSessionID
	}

	reverseAncestry(lineage)
	return lineage, nil
}

// SaveAncestry persists existing ancestry metadata for portable session
// imports.
func (s *SQLiteStore) SaveAncestry(ctx context.Context, record SessionAncestry) error {
	if err := validateSessionAncestry(record); err != nil {
		return err
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if err := saveSQLiteAncestryTx(ctx, tx, record); err != nil {
		return err
	}
	return tx.Commit()
}

func ensureRootAncestryTx(ctx context.Context, exec interface {
	ExecContext(context.Context, string, ...any) (sql.Result, error)
}, sessionID string, createdAt time.Time,
) error {
	_, err := exec.ExecContext(
		ctx,
		`INSERT OR IGNORE INTO session_ancestry
		 (session_id, parent_session_id, fork_point_event_id, branch_label, fork_reason, depth, created_at)
		 VALUES (?, '', '', '', '', 0, ?)`,
		sessionID,
		createdAt.Format(time.RFC3339Nano),
	)
	return err
}

func ensureSQLiteRootAncestryTx(
	ctx context.Context,
	tx *sql.Tx,
	sessionID string,
	createdAt time.Time,
) (int, error) {
	if err := ensureRootAncestryTx(ctx, tx, sessionID, createdAt); err != nil {
		return 0, err
	}
	row := tx.QueryRowContext(
		ctx,
		"SELECT depth FROM session_ancestry WHERE session_id = ?",
		sessionID,
	)
	var depth int
	if err := row.Scan(&depth); err != nil {
		return 0, err
	}
	return depth, nil
}

func saveSQLiteAncestryTx(ctx context.Context, tx *sql.Tx, record SessionAncestry) error {
	_, err := tx.ExecContext(
		ctx,
		`INSERT INTO session_ancestry
		 (session_id, parent_session_id, fork_point_event_id, branch_label, fork_reason, depth, created_at)
		 VALUES (?, ?, ?, ?, ?, ?, ?)
		 ON CONFLICT(session_id) DO NOTHING`,
		record.SessionID,
		record.ParentSessionID,
		record.ForkPointEventID,
		record.BranchLabel,
		record.ForkReason,
		record.Depth,
		record.CreatedAt.Format(time.RFC3339Nano),
	)
	return err
}

func (s *SQLiteStore) loadSQLiteAncestry(
	ctx context.Context,
	sessionID string,
) (*SessionAncestry, error) {
	row := s.db.QueryRowContext(
		ctx,
		`SELECT session_id, parent_session_id, fork_point_event_id, branch_label, fork_reason, depth, created_at
		 FROM session_ancestry
		 WHERE session_id = ?`,
		sessionID,
	)
	record, err := scanSQLiteAncestry(row)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, fmt.Errorf("session ancestry %q not found", sessionID)
		}
		return nil, err
	}
	return &record, nil
}

type ancestryScanner interface {
	Scan(dest ...any) error
}

func scanSQLiteAncestry(scanner ancestryScanner) (SessionAncestry, error) {
	var record SessionAncestry
	var createdAt string
	if err := scanner.Scan(
		&record.SessionID,
		&record.ParentSessionID,
		&record.ForkPointEventID,
		&record.BranchLabel,
		&record.ForkReason,
		&record.Depth,
		&createdAt,
	); err != nil {
		return SessionAncestry{}, err
	}
	ts, err := time.Parse(time.RFC3339Nano, createdAt)
	if err != nil {
		return SessionAncestry{}, err
	}
	record.CreatedAt = ts
	return record, nil
}
