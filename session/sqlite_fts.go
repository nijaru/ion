package session

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
	"time"
)

// SearchResult represents a matching entry found via full-text search.
type SearchResult struct {
	EntryID   string    `json:"entry_id"`
	Role      string    `json:"role"`
	Snippet   string    `json:"snippet"`
	Timestamp time.Time `json:"timestamp"`
}

// sanitizeFTSQuery prepares a user search query for SQLite FTS5 matching.
func sanitizeFTSQuery(query string) string {
	query = strings.TrimSpace(query)
	if query == "" {
		return ""
	}
	// If query has no special characters, tokenize into prefix match terms
	words := strings.Fields(query)
	var terms []string
	for _, word := range words {
		cleaned := strings.Map(func(r rune) rune {
			if r == '"' || r == '*' || r == ':' || r == '^' || r == '(' || r == ')' {
				return -1
			}
			return r
		}, word)
		if cleaned != "" {
			terms = append(terms, fmt.Sprintf("%q*", cleaned))
		}
	}
	if len(terms) == 0 {
		return ""
	}
	return strings.Join(terms, " ")
}

func (s *SQLiteStore) indexEntryFTS(ctx context.Context, tx *sql.Tx, entry Entry) error {
	role, text := extractEntryText(entry)
	if text == "" {
		return nil
	}
	_, err := tx.ExecContext(ctx, `
		INSERT INTO entries_fts(entry_id, role, content)
		VALUES(?, ?, ?)
	`, entry.ID(), role, text)
	return err
}

func extractEntryText(entry Entry) (string, string) {
	if me, ok := entry.(*MessageEntry); ok && me.Message != nil {
		switch m := me.Message.(type) {
		case *UserMessage:
			var parts []string
			for _, c := range m.Content {
				if tc, ok := c.(TextContent); ok {
					parts = append(parts, tc.Text)
				}
			}
			return "user", strings.Join(parts, "\n")
		case *AssistantMessage:
			var parts []string
			for _, c := range m.Content {
				if tc, ok := c.(TextContent); ok {
					parts = append(parts, tc.Text)
				} else if tc, ok := c.(ThinkingContent); ok {
					parts = append(parts, tc.Text)
				}
			}
			return "assistant", strings.Join(parts, "\n")
		case *ToolResultMessage:
			var parts []string
			for _, c := range m.Content {
				if tc, ok := c.(TextContent); ok {
					parts = append(parts, tc.Text)
				}
			}
			return "tool_result", strings.Join(parts, "\n")
		}
	} else if comp, ok := entry.(*CompactionEntry); ok {
		return "compaction", comp.Summary
	} else if sum, ok := entry.(*BranchSummaryEntry); ok {
		return "summary", sum.Summary
	}
	return "", ""
}

// SearchEntries searches the conversation entry text using SQLite FTS5.
func (s *SQLiteStore) SearchEntries(ctx context.Context, query string, limit int) ([]SearchResult, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.closed || s.db == nil {
		return nil, ErrSessionClosed
	}

	ftsQuery := sanitizeFTSQuery(query)
	if ftsQuery == "" {
		return nil, nil
	}
	if limit <= 0 {
		limit = 20
	}

	rows, err := s.db.QueryContext(ctx, `
		SELECT fts.entry_id, fts.role, snippet(entries_fts, 2, '<b>', '</b>', '...', 16), e.timestamp
		FROM entries_fts fts
		JOIN entries e ON e.id = fts.entry_id
		WHERE entries_fts MATCH ?
		ORDER BY rank
		LIMIT ?
	`, ftsQuery, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var results []SearchResult
	for rows.Next() {
		var res SearchResult
		var ts int64
		if err := rows.Scan(&res.EntryID, &res.Role, &res.Snippet, &ts); err != nil {
			return nil, err
		}
		res.Timestamp = time.UnixMilli(ts)
		results = append(results, res)
	}
	return results, rows.Err()
}

// SearchEntries searches conversation entries on the session façade.
func (s *sessionImpl) SearchEntries(ctx context.Context, query string, limit int) ([]SearchResult, error) {
	return s.store.SearchEntries(ctx, query, limit)
}
