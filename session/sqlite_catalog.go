package session

import (
	"context"
	"fmt"
	"time"
)

// Session catalog and input-history persistence.
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
	rows, err := s.db.QueryContext(
		ctx,
		"SELECT session_id, leaf_id, workdir, model, branch, name, summary, last_preview, updated_at FROM sessions WHERE workdir = ? ORDER BY updated_at DESC",
		workdir,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var sessions []SessionInfoEntry
	for rows.Next() {
		var sid, leafID, wd, model, branch, name, summary, preview string
		var updatedAt int64
		if err := rows.Scan(&sid, &leafID, &wd, &model, &branch, &name, &summary, &preview, &updatedAt); err != nil {
			return nil, err
		}
		if leafID == "" {
			leafID = sid
		}
		sessions = append(sessions, SessionInfoEntry{
			EntryBase:   EntryBase{ID: sid, Timestamp: time.Unix(updatedAt, 0)},
			LeafID:      leafID,
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
	if _, err := tx.ExecContext(
		ctx,
		`
		INSERT INTO sessions (session_id, leaf_id, workdir, model, branch, name, summary, last_preview, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(session_id) DO UPDATE SET
			leaf_id = CASE WHEN excluded.leaf_id <> '' THEN excluded.leaf_id ELSE sessions.leaf_id END,
			workdir = excluded.workdir,
			model = excluded.model,
			branch = excluded.branch,
			name = excluded.name,
			summary = excluded.summary,
			last_preview = excluded.last_preview,
			updated_at = excluded.updated_at`,
		info.ID(),
		info.LeafID,
		info.Workdir,
		info.Model,
		info.Branch,
		info.Name,
		info.Summary,
		info.LastPreview,
		updatedAt.Unix(),
	); err != nil {
		return classifySQLiteError("update session catalog", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit session catalog", err)
	}
	return nil
}

// GetSessionInfo returns one catalog record by stable session identity or its
// selected leaf checkpoint. The latter lookup keeps runtime replacement and
// direct leaf-oriented commands independent from catalog identity details.
func (s *SQLiteStore) GetSessionInfo(ctx context.Context, id string) (SessionInfoEntry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return SessionInfoEntry{}, err
	}
	var info SessionInfoEntry
	var updatedAt int64
	err := s.db.QueryRowContext(
		ctx,
		sessionCatalogLookupSQL,
		id,
		id,
		id,
		id,
	).Scan(
		&info.EntryBase.ID,
		&info.LeafID,
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
	if info.LeafID == "" {
		info.LeafID = info.ID()
	}
	info.EntryBase.Timestamp = time.Unix(updatedAt, 0)
	info.UpdatedAt = info.EntryBase.Timestamp
	return info, nil
}
