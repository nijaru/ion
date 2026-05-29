package session

import "context"

// Search searches the event log using FTS5.
func (s *SQLiteStore) Search(ctx context.Context, sessionID string, query string) ([]Event, error) {
	rows, err := s.db.QueryContext(
		ctx,
		`SELECT e.id, e.session_id, COALESCE(e.turn_id, ''), e.seq, COALESCE(e.parent_id, ''), e.type, e.timestamp, e.data, e.metadata, e.cost
		 FROM events e
		 JOIN events_fts f ON f.rowid = e.rowid
		 WHERE e.session_id = ? AND f.content MATCH ?
		 ORDER BY e.rowid ASC`,
		sessionID,
		query,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanEventRows(rows)
}
