package session

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

// TurnState describes the durable lifecycle of one logical agent turn.
// Interrupted is produced only during startup recovery; it is never treated
// as committed conversation history.
type TurnState string

const (
	TurnStarted     TurnState = "started"
	TurnCommitted   TurnState = "committed"
	TurnAborted     TurnState = "aborted"
	TurnInterrupted TurnState = "interrupted"
)

// TurnRecord is durable runtime evidence, separate from model-visible session
// messages. Entries belonging to a non-committed turn remain available for
// recovery inspection but are excluded from Branch and BuildContext.
type TurnRecord struct {
	ID          string
	Sequence    int64
	State       TurnState
	LeafID      string
	Input       string
	InputImages []ImageContent
	ContextID   string
	StartedAt   time.Time
	EndedAt     time.Time
	Error       string
}

// DurableStore is the storage boundary needed by the runtime controller. It
// intentionally extends the ordinary tree store instead of adding a second
// persistence implementation or a compatibility protocol.
type DurableStore interface {
	Store
	BeginTurn(
		ctx context.Context,
		turnID, input string,
		inputImages []ImageContent,
		contextID string,
	) (TurnRecord, error)
	GetTurn(ctx context.Context, turnID string) (TurnRecord, error)
	AppendTurnEntry(ctx context.Context, turnID string, entry Entry) (string, error)
	TurnBranch(ctx context.Context, turnID string) ([]Entry, error)
	CommitTurn(ctx context.Context, turnID string) error
	AbortTurn(ctx context.Context, turnID string, reason string) error
	InterruptedTurns(ctx context.Context) ([]TurnRecord, error)
}

// TurnBranch returns the selected branch including entries staged by the
// specified started turn. It is for the owning runtime while a turn is active;
// ordinary Branch and replay intentionally hide uncommitted entries.
func (s *SQLiteStore) TurnBranch(ctx context.Context, turnID string) ([]Entry, error) {
	ctx = normalizeContext(ctx)
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return nil, err
	}
	record, err := scanTurnRecord(s.db.QueryRowContext(ctx, `
		SELECT turn_id, sequence, state, leaf_id, input, input_images, context_id, started_at, ended_at, error
		FROM turns WHERE turn_id = ?`, turnID))
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("%w: %s", ErrTurnNotFound, turnID)
		}
		return nil, fmt.Errorf("read turn %q: %w", turnID, err)
	}
	if record.State != TurnStarted && record.State != TurnCommitted {
		return nil, fmt.Errorf("%w: turn %q is %s", ErrTurnState, turnID, record.State)
	}
	if record.LeafID == "" {
		return nil, nil
	}
	rows, err := s.db.QueryContext(ctx, `
		WITH RECURSIVE branch(id, parent_id, depth, path) AS (
			SELECT e.id, e.parent_id, 0, '/' || hex(e.id) || '/'
			FROM entries e
			WHERE e.id = ?
			  AND (e.turn_id IS NULL OR e.turn_id = ? OR EXISTS (
				SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
			  ))
			UNION ALL
			SELECT e.id, e.parent_id, branch.depth + 1, branch.path || hex(e.id) || '/'
			FROM entries e
			JOIN branch ON e.id = branch.parent_id
			WHERE (e.turn_id IS NULL OR e.turn_id = ? OR EXISTS (
				SELECT 1 FROM turns t WHERE t.turn_id = e.turn_id AND t.state = 'committed'
			  ))
			  AND instr(branch.path, '/' || hex(e.id) || '/') = 0
		)
		SELECT e.id, e.parent_id, e.type, e.timestamp, e.payload
		FROM branch
		JOIN entries e ON e.id = branch.id
		ORDER BY branch.depth DESC`, record.LeafID, turnID, turnID)
	if err != nil {
		return nil, fmt.Errorf("load turn branch %q: %w", turnID, err)
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
	if err := validateBranchRoot(entries, record.LeafID); err != nil {
		return nil, err
	}
	return entries, nil
}

var _ DurableStore = (*SQLiteStore)(nil)

// GetTurn reads authoritative lifecycle evidence for one durable turn. It is
// separate from replay: started, aborted, and interrupted turns remain
// inspectable even though only committed entries enter the session branch.
func (s *SQLiteStore) GetTurn(ctx context.Context, turnID string) (TurnRecord, error) {
	ctx = normalizeContext(ctx)
	if strings.TrimSpace(turnID) == "" {
		return TurnRecord{}, fmt.Errorf("turn ID is required")
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return TurnRecord{}, err
	}
	record, err := scanTurnRecord(s.db.QueryRowContext(ctx, `
		SELECT turn_id, sequence, state, leaf_id, input, input_images, context_id, started_at, ended_at, error
		FROM turns WHERE turn_id = ?`, turnID))
	if err == sql.ErrNoRows {
		return TurnRecord{}, fmt.Errorf("%w: %s", ErrTurnNotFound, turnID)
	}
	if err != nil {
		return TurnRecord{}, fmt.Errorf("read turn %q: %w", turnID, err)
	}
	return record, nil
}

// LatestTurn returns the most recently started durable turn. It is intended
// for recovery and diagnostics after a runtime has settled; callers that know
// the turn identity should use GetTurn instead.
func (s *SQLiteStore) LatestTurn(ctx context.Context) (TurnRecord, error) {
	ctx = normalizeContext(ctx)
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return TurnRecord{}, err
	}
	record, err := scanTurnRecord(s.db.QueryRowContext(ctx, `
		SELECT turn_id, sequence, state, leaf_id, input, input_images, context_id, started_at, ended_at, error
		FROM turns ORDER BY sequence DESC LIMIT 1`))
	if err == sql.ErrNoRows {
		return TurnRecord{}, fmt.Errorf("%w: no turns", ErrTurnNotFound)
	}
	if err != nil {
		return TurnRecord{}, fmt.Errorf("read latest turn: %w", err)
	}
	return record, nil
}

func normalizeContext(ctx context.Context) context.Context {
	if ctx == nil {
		return context.Background()
	}
	return ctx
}

func recoverInterruptedTurns(ctx context.Context, db *sql.DB) error {
	ctx = normalizeContext(ctx)
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return classifySQLiteError("begin interrupted-turn recovery", err)
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx, `
		UPDATE turns
		SET state = 'interrupted', ended_at = ?
		WHERE state = 'started'`, time.Now().UnixMilli()); err != nil {
		return classifySQLiteError("recover interrupted turns", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit interrupted-turn recovery", err)
	}
	return nil
}

func (s *SQLiteStore) BeginTurn(
	ctx context.Context,
	turnID, input string,
	inputImages []ImageContent,
	contextID string,
) (TurnRecord, error) {
	ctx = normalizeContext(ctx)
	if turnID == "" {
		turnID = newID()
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return TurnRecord{}, err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return TurnRecord{}, err
	}
	var existing string
	err = tx.QueryRowContext(ctx, "SELECT turn_id FROM turns WHERE turn_id = ?", turnID).Scan(&existing)
	if err == nil {
		_ = tx.Rollback()
		return TurnRecord{}, fmt.Errorf("%w: %s", ErrTurnState, turnID)
	}
	if err != sql.ErrNoRows {
		_ = tx.Rollback()
		return TurnRecord{}, fmt.Errorf("look up turn %q: %w", turnID, err)
	}
	sequence, err := nextSequenceTx(ctx, tx)
	if err != nil {
		_ = tx.Rollback()
		return TurnRecord{}, err
	}
	started := time.Now()
	leaf := s.leaf
	encodedImages, err := json.Marshal(inputImages)
	if err != nil {
		_ = tx.Rollback()
		return TurnRecord{}, fmt.Errorf("encode turn input images: %w", err)
	}
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO turns(turn_id,sequence,state,leaf_id,input,input_images,context_id,started_at)
		VALUES(?,?,?,?,?,?,?,?)`, turnID, sequence, string(TurnStarted), leaf, input, encodedImages, contextID, started.UnixMilli()); err != nil {
		_ = tx.Rollback()
		return TurnRecord{}, classifySQLiteError("insert turn begin", err)
	}
	if err := tx.Commit(); err != nil {
		return TurnRecord{}, classifySQLiteError("commit turn begin", err)
	}
	return TurnRecord{
		ID: turnID, Sequence: sequence, State: TurnStarted, LeafID: leaf,
		Input: input, InputImages: cloneImageContents(inputImages), ContextID: contextID, StartedAt: started,
	}, nil
}

func (s *SQLiteStore) AppendTurnEntry(ctx context.Context, turnID string, entry Entry) (string, error) {
	ctx = normalizeContext(ctx)
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return "", err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return "", err
	}
	record, err := turnRecordTx(ctx, tx, turnID)
	if err != nil {
		_ = tx.Rollback()
		return "", err
	}
	if record.State != TurnStarted {
		_ = tx.Rollback()
		return "", fmt.Errorf("%w: append to turn %q in state %s", ErrTurnState, turnID, record.State)
	}
	if entry == nil || entry.ID() == "" {
		_ = tx.Rollback()
		return "", fmt.Errorf("entry ID is required")
	}
	entryType, entryPayload, err := encodeEntry(entry)
	if err != nil {
		_ = tx.Rollback()
		return "", err
	}
	var (
		existingTurn    sql.NullString
		existingParent  string
		existingType    string
		existingTS      int64
		existingPayload []byte
	)
	lookupErr := tx.QueryRowContext(ctx, `
		SELECT turn_id, parent_id, type, timestamp, payload
		FROM entries WHERE id = ?`, entry.ID()).Scan(
		&existingTurn, &existingParent, &existingType, &existingTS, &existingPayload,
	)
	if lookupErr == nil {
		if existingTurn.Valid && existingTurn.String == turnID {
			if existingParent == entry.ParentID() &&
				existingType == entryType &&
				existingTS == entry.When().UnixMilli() &&
				bytes.Equal(existingPayload, entryPayload) {
				_ = tx.Rollback()
				return entry.ID(), nil
			}
			_ = tx.Rollback()
			return "", fmt.Errorf(
				"%w: entry %q was already recorded with different content",
				ErrTurnEntryConflict,
				entry.ID(),
			)
		}
		_ = tx.Rollback()
		return "", fmt.Errorf("entry %q already exists", entry.ID())
	}
	if lookupErr != sql.ErrNoRows {
		_ = tx.Rollback()
		return "", fmt.Errorf("check turn entry %q: %w", entry.ID(), lookupErr)
	}
	if entry.ParentID() != record.LeafID {
		_ = tx.Rollback()
		return "", fmt.Errorf(
			"entry %q parent %q does not match turn leaf %q",
			entry.ID(),
			entry.ParentID(),
			record.LeafID,
		)
	}
	id, err := s.appendTx(ctx, tx, turnID, entry)
	if err != nil {
		_ = tx.Rollback()
		return "", err
	}
	if _, err := tx.ExecContext(ctx, "UPDATE turns SET leaf_id = ? WHERE turn_id = ?", id, turnID); err != nil {
		_ = tx.Rollback()
		return "", fmt.Errorf("advance turn leaf: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return "", classifySQLiteError("commit turn entry", err)
	}
	return id, nil
}

func (s *SQLiteStore) CommitTurn(ctx context.Context, turnID string) error {
	ctx = normalizeContext(ctx)
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return err
	}
	record, err := turnRecordTx(ctx, tx, turnID)
	if err != nil {
		_ = tx.Rollback()
		return err
	}
	if record.State == TurnCommitted {
		_ = tx.Rollback()
		return nil
	}
	if record.State != TurnStarted {
		_ = tx.Rollback()
		return fmt.Errorf("%w: commit turn %q in state %s", ErrTurnState, turnID, record.State)
	}
	ended := time.Now()
	if _, err := tx.ExecContext(ctx,
		"UPDATE turns SET state = ?, ended_at = ? WHERE turn_id = ?",
		string(TurnCommitted), ended.UnixMilli(), turnID); err != nil {
		_ = tx.Rollback()
		return fmt.Errorf("commit turn record: %w", err)
	}
	if err := setLeafTx(ctx, tx, record.LeafID); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit turn", err)
	}
	s.leaf = record.LeafID
	return nil
}

func (s *SQLiteStore) AbortTurn(ctx context.Context, turnID string, reason string) error {
	ctx = normalizeContext(ctx)
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return err
	}
	record, err := turnRecordTx(ctx, tx, turnID)
	if err != nil {
		_ = tx.Rollback()
		return err
	}
	if record.State == TurnAborted {
		if strings.TrimSpace(reason) == "" || record.Error == reason {
			_ = tx.Rollback()
			return nil
		}
		if _, err := tx.ExecContext(ctx,
			"UPDATE turns SET error = ? WHERE turn_id = ? AND state = ?",
			reason, turnID, string(TurnAborted)); err != nil {
			_ = tx.Rollback()
			return fmt.Errorf("update aborted turn reason: %w", err)
		}
		if err := tx.Commit(); err != nil {
			return classifySQLiteError("commit aborted turn reason", err)
		}
		return nil
	}
	if record.State == TurnCommitted {
		_ = tx.Rollback()
		return fmt.Errorf("%w: committed turn %q cannot be aborted", ErrTurnState, turnID)
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE turns SET state = ?, ended_at = ?, error = ? WHERE turn_id = ?`,
		string(TurnAborted), time.Now().UnixMilli(), reason, turnID); err != nil {
		_ = tx.Rollback()
		return fmt.Errorf("abort turn record: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit turn abort", err)
	}
	return nil
}

func (s *SQLiteStore) InterruptedTurns(ctx context.Context) ([]TurnRecord, error) {
	ctx = normalizeContext(ctx)
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.closed || s.db == nil {
		return nil, ErrSessionClosed
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT turn_id, sequence, state, leaf_id, input, input_images, context_id, started_at, ended_at, error
		FROM turns WHERE state = ? ORDER BY sequence ASC`, string(TurnInterrupted))
	if err != nil {
		return nil, fmt.Errorf("list interrupted turns: %w", err)
	}
	defer rows.Close()
	var records []TurnRecord
	for rows.Next() {
		record, err := scanTurnRecord(rows)
		if err != nil {
			return nil, fmt.Errorf("decode interrupted turn: %w", err)
		}
		record.State = TurnInterrupted
		records = append(records, record)
	}
	return records, rows.Err()
}

func turnRecordTx(ctx context.Context, tx *sql.Tx, turnID string) (TurnRecord, error) {
	row := tx.QueryRowContext(ctx, `
		SELECT turn_id, sequence, state, leaf_id, input, input_images, context_id, started_at, ended_at, error
		FROM turns WHERE turn_id = ?`, turnID)
	record, err := scanTurnRecord(row)
	if err == sql.ErrNoRows {
		return TurnRecord{}, fmt.Errorf("%w: %s", ErrTurnNotFound, turnID)
	}
	if err != nil {
		return TurnRecord{}, fmt.Errorf("read turn %q: %w", turnID, err)
	}
	return record, nil
}

func scanTurnRecord(row interface{ Scan(...any) error }) (TurnRecord, error) {
	var (
		id, state, leaf, input, contextID, reason string
		inputImages                               []byte
		sequence, started, ended                  int64
	)
	if err := row.Scan(
		&id,
		&sequence,
		&state,
		&leaf,
		&input,
		&inputImages,
		&contextID,
		&started,
		&ended,
		&reason,
	); err != nil {
		return TurnRecord{}, err
	}
	var images []ImageContent
	if len(inputImages) > 0 {
		if err := json.Unmarshal(inputImages, &images); err != nil {
			return TurnRecord{}, fmt.Errorf("decode turn input images: %w", err)
		}
	}
	record := TurnRecord{
		ID: id, Sequence: sequence, State: TurnState(state), LeafID: leaf,
		Input: input, InputImages: images, ContextID: contextID,
		StartedAt: time.UnixMilli(started), Error: reason,
	}
	if ended != 0 {
		record.EndedAt = time.UnixMilli(ended)
	}
	return record, nil
}

func cloneImageContents(images []ImageContent) []ImageContent {
	if len(images) == 0 {
		return nil
	}
	cloned := make([]ImageContent, len(images))
	for i, image := range images {
		cloned[i] = ImageContent{Data: append([]byte(nil), image.Data...), MimeType: image.MimeType}
	}
	return cloned
}
