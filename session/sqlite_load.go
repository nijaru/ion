package session

import (
	"context"
	"database/sql"
	"errors"

	"github.com/oklog/ulid/v2"
)

// Load reconstructs a session from the database.
func (s *SQLiteStore) Load(ctx context.Context, sessionID string) (*Session, error) {
	rows, err := s.db.QueryContext(
		ctx,
		"SELECT id, session_id, COALESCE(turn_id, ''), seq, COALESCE(parent_id, ''), type, timestamp, data, metadata, cost FROM events WHERE session_id = ? ORDER BY rowid ASC",
		sessionID,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	return loadSessionRows(sessionID, s, rows)
}

// EventsAfter returns durable events with sequence numbers greater than afterSeq.
func (s *SQLiteStore) EventsAfter(
	ctx context.Context,
	sessionID string,
	afterSeq int64,
) ([]Event, error) {
	rows, err := s.db.QueryContext(
		ctx,
		`SELECT id, session_id, COALESCE(turn_id, ''), seq, COALESCE(parent_id, ''), type, timestamp, data, metadata, cost
		   FROM events
		  WHERE session_id = ? AND seq > ?
		  ORDER BY seq ASC, rowid ASC`,
		sessionID,
		afterSeq,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanEventRows(rows)
}

// LoadUntil loads a session up to (and including) the given event ID.
func (s *SQLiteStore) LoadUntil(
	ctx context.Context,
	sessionID string,
	eventID ulid.ULID,
) (*Session, error) {
	row := s.db.QueryRowContext(
		ctx,
		"SELECT rowid FROM events WHERE session_id = ? AND id = ?",
		sessionID,
		eventID.String(),
	)
	var targetRowID int64
	err := row.Scan(&targetRowID)
	var rows *sql.Rows
	switch {
	case err == nil:
		rows, err = s.db.QueryContext(
			ctx,
			"SELECT id, session_id, COALESCE(turn_id, ''), seq, COALESCE(parent_id, ''), type, timestamp, data, metadata, cost FROM events WHERE session_id = ? AND rowid <= ? ORDER BY rowid ASC",
			sessionID,
			targetRowID,
		)
	case errors.Is(err, sql.ErrNoRows):
		rows, err = s.db.QueryContext(
			ctx,
			"SELECT id, session_id, COALESCE(turn_id, ''), seq, COALESCE(parent_id, ''), type, timestamp, data, metadata, cost FROM events WHERE session_id = ? AND id <= ? ORDER BY rowid ASC",
			sessionID,
			eventID.String(),
		)
	default:
		return nil, err
	}
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	return loadSessionRows(sessionID, s, rows)
}

func scanEventRows(rows *sql.Rows) ([]Event, error) {
	var events []Event
	for rows.Next() {
		e, err := scanEventRow(rows)
		if err != nil {
			return nil, err
		}
		events = append(events, e)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return events, nil
}

func scanEventRow(rows *sql.Rows) (Event, error) {
	var idStr, turnID, parentID, typeStr, timeStr string
	var loadedSessionID string
	var seq int64
	var data, metadata []byte
	var cost float64
	if err := rows.Scan(
		&idStr,
		&loadedSessionID,
		&turnID,
		&seq,
		&parentID,
		&typeStr,
		&timeStr,
		&data,
		&metadata,
		&cost,
	); err != nil {
		return Event{}, err
	}
	return decodeEventRow(
		idStr,
		loadedSessionID,
		turnID,
		parentID,
		typeStr,
		timeStr,
		seq,
		data,
		metadata,
		cost,
	)
}

func loadSessionRows(sessionID string, store *SQLiteStore, rows *sql.Rows) (*Session, error) {
	replayer := NewReplayer()
	sess := replayer.NewSession(sessionID).WithWriter(store)
	for rows.Next() {
		e, err := scanEventRow(rows)
		if err != nil {
			return nil, err
		}
		if err := replayer.Apply(sess, e); err != nil {
			return nil, err
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return sess, nil
}
