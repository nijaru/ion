package session

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"time"
)

// Durable entry persistence, tree navigation, and entry codecs.
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
	_, err = tx.ExecContext(
		ctx,
		"INSERT INTO entries(id,parent_id,type,timestamp,sequence,turn_id,payload) VALUES(?,?,?,?,?,?,?)",
		id, parentID, typ, ts, sequence, nullableString(turnID), payload,
	)
	if err != nil {
		return "", classifySQLiteError("insert entry", err)
	}
	_ = s.indexEntryFTS(ctx, tx, entry)
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

// PersistImportedSession atomically stores imported entries and their catalog
// metadata without changing the active session selection. Runtime replacement
// owns activation, so a failed import or fork cannot move the current runtime.
func (s *SQLiteStore) PersistImportedSession(
	ctx context.Context,
	entries []Entry,
	info SessionInfoEntry,
) error {
	if info.ID() == "" {
		return errors.New("imported session identity is required")
	}
	if info.LeafID == "" {
		return errors.New("imported session leaf is required")
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
	for _, entry := range entries {
		if _, err := s.appendTx(ctx, tx, "", entry); err != nil {
			return fmt.Errorf("persist imported entry: %w", err)
		}
	}
	if _, err := s.getVisibleEntryTx(ctx, tx, info.LeafID); err != nil {
		return fmt.Errorf("validate imported leaf %q: %w", info.LeafID, err)
	}
	updatedAt := info.UpdatedAt
	if updatedAt.IsZero() {
		updatedAt = time.Now()
	}
	if _, err := tx.ExecContext(
		ctx,
		`INSERT INTO sessions (session_id, leaf_id, workdir, model, branch, name, summary, last_preview, updated_at)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
			ON CONFLICT(session_id) DO UPDATE SET
				leaf_id = excluded.leaf_id,
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
		return classifySQLiteError("persist imported session", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit imported session", err)
	}
	return nil
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
	if entryID != "" {
		if _, err := s.getVisibleEntryTx(ctx, tx, entryID); err != nil {
			_ = tx.Rollback()
			return "", fmt.Errorf("move to %q: %w", entryID, err)
		}
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
			Usage:     summary.Usage,
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

// BranchSeq yields entries from root to current leaf as a zero-allocation iterator.
func (s *SQLiteStore) BranchSeq(ctx context.Context) iter.Seq2[Entry, error] {
	return func(yield func(Entry, error) bool) {
		s.mu.RLock()
		if s.closed || s.db == nil {
			s.mu.RUnlock()
			yield(nil, ErrSessionClosed)
			return
		}
		leaf := s.leaf
		s.mu.RUnlock()

		for e, err := range s.BranchAtSeq(ctx, leaf) {
			if !yield(e, err) {
				return
			}
		}
	}
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

// BranchAtSeq yields entries on the selected leaf path directly from SQLite rows.
func (s *SQLiteStore) BranchAtSeq(ctx context.Context, leafID string) iter.Seq2[Entry, error] {
	return func(yield func(Entry, error) bool) {
		if leafID == "" {
			return
		}
		s.mu.RLock()
		if s.closed || s.db == nil {
			s.mu.RUnlock()
			yield(nil, ErrSessionClosed)
			return
		}
		db := s.db
		s.mu.RUnlock()

		rows, err := db.QueryContext(ctx, `
			WITH RECURSIVE branch(id, parent_id, depth, path) AS (
				SELECT e.id, e.parent_id, 0, '/' || hex(e.id) || '/'
				FROM entries e
				WHERE e.id = ?
				  AND (e.turn_id IS NULL OR EXISTS (
					SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
				  ))
				UNION ALL
				SELECT e.id, e.parent_id, branch.depth + 1, branch.path || hex(e.id) || '/'
				FROM entries e
				JOIN branch ON e.id = branch.parent_id
				WHERE (e.turn_id IS NULL OR EXISTS (
					SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
				))
				  AND instr(branch.path, '/' || hex(e.id) || '/') = 0
			)
			SELECT e.id, e.parent_id, e.type, e.timestamp, e.payload
			FROM branch
			JOIN entries e ON e.id = branch.id
			ORDER BY branch.depth DESC
		`, leafID)
		if err != nil {
			yield(nil, err)
			return
		}
		defer rows.Close()

		first := true
		count := 0
		for rows.Next() {
			entry, err := scanEntry(rows)
			if err != nil {
				yield(nil, err)
				return
			}
			if first {
				if parentID := entry.ParentID(); parentID != "" {
					yield(nil, fmt.Errorf(
						"%w: branch leaf %q stops at entry %q with missing or cyclic parent %q",
						ErrCorruptSession,
						leafID,
						entry.ID(),
						parentID,
					))
					return
				}
				first = false
			}
			count++
			if !yield(entry, nil) {
				return
			}
		}
		if err := rows.Err(); err != nil {
			yield(nil, err)
			return
		}
		if count == 0 {
			yield(nil, sql.ErrNoRows)
		}
	}
}

func (s *SQLiteStore) branchAtLocked(ctx context.Context, leafID string) ([]Entry, error) {
	if leafID == "" {
		return nil, nil
	}

	// Reconstruct the parent chain and decode its rows in one query. The path
	// guard keeps malformed cycles from spinning forever. IDs are hex-encoded
	// inside the path so recursion remains safe for opaque imported IDs.
	rows, err := s.db.QueryContext(ctx, `
		WITH RECURSIVE branch(id, parent_id, depth, path) AS (
			SELECT e.id, e.parent_id, 0, '/' || hex(e.id) || '/'
			FROM entries e
			WHERE e.id = ?
			  AND (e.turn_id IS NULL OR EXISTS (
				SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
			  ))
			UNION ALL
			SELECT e.id, e.parent_id, branch.depth + 1, branch.path || hex(e.id) || '/'
			FROM entries e
			JOIN branch ON e.id = branch.parent_id
			WHERE (e.turn_id IS NULL OR EXISTS (
				SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
			))
			  AND instr(branch.path, '/' || hex(e.id) || '/') = 0
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
	if err := validateBranchRoot(entries, leafID); err != nil {
		return nil, err
	}
	return entries, nil
}

func validateBranchRoot(entries []Entry, leafID string) error {
	if len(entries) == 0 {
		return fmt.Errorf("%w: branch leaf %q has no entries", ErrCorruptSession, leafID)
	}
	if parentID := entries[0].ParentID(); parentID != "" {
		return fmt.Errorf(
			"%w: branch leaf %q stops at entry %q with missing or cyclic parent %q",
			ErrCorruptSession,
			leafID,
			entries[0].ID(),
			parentID,
		)
	}
	return nil
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

// EntriesSeq yields all entries in the session directly from SQLite rows.
func (s *SQLiteStore) EntriesSeq(ctx context.Context) iter.Seq2[Entry, error] {
	return func(yield func(Entry, error) bool) {
		s.mu.RLock()
		if s.closed || s.db == nil {
			s.mu.RUnlock()
			yield(nil, ErrSessionClosed)
			return
		}
		db := s.db
		s.mu.RUnlock()

		rows, err := db.QueryContext(ctx, `
			SELECT e.id,e.parent_id,e.type,e.timestamp,e.payload
			FROM entries e
			WHERE e.turn_id IS NULL OR EXISTS (
				SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
			)
			ORDER BY e.sequence ASC, e.rowid ASC`)
		if err != nil {
			yield(nil, err)
			return
		}
		defer rows.Close()

		for rows.Next() {
			e, err := scanEntry(rows)
			if err != nil {
				yield(nil, err)
				return
			}
			if !yield(e, nil) {
				return
			}
		}
		if err := rows.Err(); err != nil {
			yield(nil, err)
		}
	}
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
	Usage        Usage  `json:"usage,omitzero"`

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
		p.Usage = e.Usage
		p.Details = e.Details
	case *BranchSummaryEntry:
		typ = "branch_summary"
		p.FromID = e.FromID
		p.Summary = e.Summary
		p.Usage = e.Usage
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
		return &CompactionEntry{
			EntryBase:    base,
			Summary:      p.Summary,
			FirstKeptID:  p.FirstKeptID,
			TokensBefore: p.TokensBefore,
			Usage:        p.Usage,
			Details:      p.Details,
		}, nil
	case "branch_summary":
		return &BranchSummaryEntry{
			EntryBase: base,
			FromID:    p.FromID,
			Summary:   p.Summary,
			Usage:     p.Usage,
			Details:   p.Details,
		}, nil
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
		return &CustomMessageEntry{
			EntryBase:  base,
			CustomType: p.CustomMessageType,
			Content:    content,
			Display:    p.CustomMessageDisplay,
			Details:    p.CustomMessageDetails,
		}, nil
	default:
		return nil, fmt.Errorf("unknown entry type %q", typ)
	}
}
