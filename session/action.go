package session

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"
)

// ActionState is the durable lifecycle of one externally observable action.
// Action records are runtime evidence, not model-visible conversation entries.
type ActionState string

const (
	ActionPrepared      ActionState = "prepared"
	ActionAuthorized    ActionState = "authorized"
	ActionStarted       ActionState = "started"
	ActionCompleted     ActionState = "completed"
	ActionFailed        ActionState = "failed"
	ActionCancelled     ActionState = "cancelled"
	ActionDenied        ActionState = "denied"
	ActionIndeterminate ActionState = "indeterminate"
)

// ActionAuthorization is the policy result recorded against the exact
// normalized action fingerprint.
type ActionAuthorization string

const (
	ActionAllow ActionAuthorization = "allow"
	ActionDeny  ActionAuthorization = "deny"
)

// ActionTransition is append-only evidence of one journal state change. The
// current action row is the fast recovery index; transitions preserve the
// audit trail that proves which durability boundaries were acknowledged.
type ActionTransition struct {
	ID        int64
	ActionID  string
	From      ActionState
	To        ActionState
	Reason    string
	Timestamp time.Time
}

// ActionRecord is the durable evidence for one logical tool effect. JSON
// payloads are stored in normalized form by the runtime action planner; the
// storage layer keeps them opaque and never interprets model content as policy.
type ActionRecord struct {
	ID            string
	InvocationID  string
	SessionID     string
	TurnID        string
	Tool          string
	Category      string
	Operation     string
	Arguments     []byte
	Metadata      []byte
	Preimages     []byte
	Fingerprint   string
	CWD           string
	Paths         []string
	Environment   []string
	NetworkIntent string
	MCPIdentity   string
	PolicyMode    string

	State          ActionState
	Authorization  ActionAuthorization
	ResultIdentity string
	Error          string
	CleanupOutcome string
	// ProcessIdentity is an opaque, host-issued identity token for the
	// process-group leader created at the effect boundary. The session layer
	// stores it but never interprets or signals it.
	ProcessIdentity string
	PreparedAt      time.Time
	AuthorizedAt    time.Time
	StartedAt       time.Time
	EndedAt         time.Time
}

// ActionJournal is the storage boundary for external-action evidence. Each
// transition is durable before the caller crosses the corresponding effect
// boundary. Implementations must make repeated identical calls idempotent and
// reject changed records or illegal transitions.
type ActionJournal interface {
	PrepareAction(ctx context.Context, record ActionRecord) (ActionRecord, error)
	AuthorizeAction(ctx context.Context, actionID, policyMode string) (ActionRecord, error)
	DenyAction(ctx context.Context, actionID, reason string) (ActionRecord, error)
	StartAction(ctx context.Context, actionID, processIdentity string) (ActionRecord, error)
	FinishAction(ctx context.Context, actionID string, state ActionState, resultIdentity, reason, cleanup string) (ActionRecord, error)
	ReconcileAction(ctx context.Context, actionID string, state ActionState, verification, resultIdentity, reason, cleanup string) (ActionRecord, error)
	RecordActionRecovery(ctx context.Context, actionID, reason, cleanup string) (ActionRecord, error)
	GetAction(ctx context.Context, actionID string) (ActionRecord, error)
	UnsettledActions(ctx context.Context) ([]ActionRecord, error)
	ActionTransitions(ctx context.Context, actionID string) ([]ActionTransition, error)
}

var (
	ErrActionNotFound = errors.New("action not found")
	ErrActionState    = errors.New("invalid action state")
	ErrActionConflict = errors.New("action identity conflict")
)

func validatePreparedAction(record ActionRecord) error {
	for name, value := range map[string]string{
		"action ID":     record.ID,
		"invocation ID": record.InvocationID,
		"session ID":    record.SessionID,
		"turn ID":       record.TurnID,
		"tool":          record.Tool,
		"operation":     record.Operation,
		"fingerprint":   record.Fingerprint,
	} {
		if value == "" {
			return fmt.Errorf("%s is required", name)
		}
	}
	if len(record.Arguments) == 0 || !json.Valid(record.Arguments) {
		return errors.New("action arguments must be valid JSON")
	}
	if record.State != "" && record.State != ActionPrepared {
		return fmt.Errorf("new action state must be prepared, got %q", record.State)
	}
	return nil
}

func cloneActionRecord(record ActionRecord) ActionRecord {
	record.Arguments = slices.Clone(record.Arguments)
	record.Metadata = slices.Clone(record.Metadata)
	record.Preimages = slices.Clone(record.Preimages)
	record.Paths = slices.Clone(record.Paths)
	record.Environment = slices.Clone(record.Environment)
	return record
}

func actionTerminal(state ActionState) bool {
	switch state {
	case ActionCompleted, ActionFailed, ActionCancelled, ActionDenied, ActionIndeterminate:
		return true
	default:
		return false
	}
}

func validActionFinishState(state ActionState) bool {
	switch state {
	case ActionCompleted, ActionFailed, ActionCancelled, ActionIndeterminate:
		return true
	default:
		return false
	}
}

var _ ActionJournal = (*SQLiteStore)(nil)

func recoverInterruptedActions(ctx context.Context, db *sql.DB) error {
	ctx = normalizeContext(ctx)
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return classifySQLiteError("begin interrupted-action recovery", err)
	}
	defer tx.Rollback()
	ended := time.Now().UnixMilli()
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO action_transitions(action_id, from_state, to_state, reason, timestamp)
		SELECT action_id, state, ?, ?, ? FROM actions WHERE state IN (?, ?)`,
		string(ActionCancelled),
		"runtime terminated before the action start boundary",
		ended, string(ActionPrepared), string(ActionAuthorized)); err != nil {
		return classifySQLiteError("record pre-start action recovery", err)
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE actions
		SET state = ?, ended_at = ?, error = CASE WHEN error = '' THEN ? ELSE error END
		WHERE state IN (?, ?)`,
		string(ActionCancelled), ended,
		"runtime terminated before the action start boundary",
		string(ActionPrepared), string(ActionAuthorized)); err != nil {
		return classifySQLiteError("recover pre-start actions", err)
	}
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO action_transitions(action_id, from_state, to_state, reason, timestamp)
		SELECT action_id, state, ?, ?, ? FROM actions WHERE state = ?`,
		string(ActionIndeterminate),
		"runtime terminated after the action start boundary without a terminal outcome",
		ended, string(ActionStarted)); err != nil {
		return classifySQLiteError("record started-action recovery", err)
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE actions
		SET state = ?, ended_at = ?, error = CASE WHEN error = '' THEN ? ELSE error END
		WHERE state = ?`,
		string(ActionIndeterminate), ended,
		"runtime terminated after the action start boundary without a terminal outcome",
		string(ActionStarted)); err != nil {
		return classifySQLiteError("recover interrupted actions", err)
	}
	if err := tx.Commit(); err != nil {
		return classifySQLiteError("commit interrupted-action recovery", err)
	}
	return nil
}

func actionIdentityEqual(a, b ActionRecord) bool {
	return a.ID == b.ID &&
		a.InvocationID == b.InvocationID &&
		a.SessionID == b.SessionID &&
		a.TurnID == b.TurnID &&
		a.Tool == b.Tool &&
		a.Category == b.Category &&
		a.Operation == b.Operation &&
		bytes.Equal(a.Arguments, b.Arguments) &&
		bytes.Equal(a.Metadata, b.Metadata) &&
		bytes.Equal(a.Preimages, b.Preimages) &&
		a.Fingerprint == b.Fingerprint &&
		a.CWD == b.CWD &&
		slices.Equal(a.Paths, b.Paths) &&
		slices.Equal(a.Environment, b.Environment) &&
		a.NetworkIntent == b.NetworkIntent &&
		a.MCPIdentity == b.MCPIdentity &&
		a.PolicyMode == b.PolicyMode
}

func encodeActionList(values []string) ([]byte, error) {
	if values == nil {
		values = []string{}
	}
	return json.Marshal(values)
}

func decodeActionList(data []byte) ([]string, error) {
	if len(data) == 0 {
		return nil, nil
	}
	var values []string
	if err := json.Unmarshal(data, &values); err != nil {
		return nil, fmt.Errorf("decode action list: %w", err)
	}
	return values, nil
}

func normalizeActionMetadata(data []byte) ([]byte, error) {
	if len(data) == 0 {
		return []byte(`{}`), nil
	}
	var value any
	if err := json.Unmarshal(data, &value); err != nil {
		return nil, errors.New("action metadata must be valid JSON")
	}
	if _, ok := value.(map[string]any); !ok {
		return nil, errors.New("action metadata must be a JSON object")
	}
	return slices.Clone(data), nil
}

func normalizeActionPreimages(data []byte) ([]byte, error) {
	if len(data) == 0 {
		return []byte(`[]`), nil
	}
	var value any
	if err := json.Unmarshal(data, &value); err != nil {
		return nil, errors.New("action preimages must be valid JSON")
	}
	if _, ok := value.([]any); !ok {
		return nil, errors.New("action preimages must be a JSON array")
	}
	return slices.Clone(data), nil
}

func actionRecordTx(ctx context.Context, tx *sql.Tx, actionID string) (ActionRecord, error) {
	row := tx.QueryRowContext(ctx, `
	SELECT action_id, invocation_id, session_id, turn_id, tool_name, category, operation, arguments, fingerprint,
	       metadata, preimages, cwd, paths, environment, network_intent, mcp_identity, policy_mode,
		       state, authorization, result_identity, error, cleanup_outcome,
		       process_identity, prepared_at, authorized_at, started_at, ended_at
		FROM actions WHERE action_id = ?`, actionID)
	record, err := scanActionRecord(row)
	if err == sql.ErrNoRows {
		return ActionRecord{}, fmt.Errorf("%w: %s", ErrActionNotFound, actionID)
	}
	if err != nil {
		return ActionRecord{}, fmt.Errorf("read action %q: %w", actionID, err)
	}
	return record, nil
}

func scanActionRecord(row interface{ Scan(...any) error }) (ActionRecord, error) {
	var (
		record                                             ActionRecord
		arguments, metadata, preimages, paths, environment []byte
		preparedAt, authorizedAt, startedAt, endedAt       int64
	)
	if err := row.Scan(
		&record.ID, &record.InvocationID, &record.SessionID, &record.TurnID,
		&record.Tool, &record.Category, &record.Operation,
		&arguments, &record.Fingerprint, &metadata, &preimages, &record.CWD, &paths, &environment,
		&record.NetworkIntent, &record.MCPIdentity, &record.PolicyMode,
		&record.State, &record.Authorization, &record.ResultIdentity,
		&record.Error, &record.CleanupOutcome, &record.ProcessIdentity,
		&preparedAt, &authorizedAt, &startedAt, &endedAt,
	); err != nil {
		return ActionRecord{}, err
	}
	var err error
	record.Arguments = slices.Clone(arguments)
	record.Metadata = slices.Clone(metadata)
	record.Preimages = slices.Clone(preimages)
	record.Paths, err = decodeActionList(paths)
	if err != nil {
		return ActionRecord{}, err
	}
	record.Environment, err = decodeActionList(environment)
	if err != nil {
		return ActionRecord{}, err
	}
	record.PreparedAt = time.UnixMilli(preparedAt)
	if authorizedAt != 0 {
		record.AuthorizedAt = time.UnixMilli(authorizedAt)
	}
	if startedAt != 0 {
		record.StartedAt = time.UnixMilli(startedAt)
	}
	if endedAt != 0 {
		record.EndedAt = time.UnixMilli(endedAt)
	}
	return record, nil
}

func (s *SQLiteStore) PrepareAction(ctx context.Context, record ActionRecord) (ActionRecord, error) {
	ctx = normalizeContext(ctx)
	if err := validatePreparedAction(record); err != nil {
		return ActionRecord{}, err
	}
	paths, err := encodeActionList(record.Paths)
	if err != nil {
		return ActionRecord{}, fmt.Errorf("encode action paths: %w", err)
	}
	environment, err := encodeActionList(record.Environment)
	if err != nil {
		return ActionRecord{}, fmt.Errorf("encode action environment: %w", err)
	}
	metadata, err := normalizeActionMetadata(record.Metadata)
	if err != nil {
		return ActionRecord{}, err
	}
	record.Metadata = metadata
	preimages, err := normalizeActionPreimages(record.Preimages)
	if err != nil {
		return ActionRecord{}, err
	}
	record.Preimages = preimages
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return ActionRecord{}, err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return ActionRecord{}, err
	}
	defer tx.Rollback()
	existing, err := actionRecordTx(ctx, tx, record.ID)
	if err == nil {
		if !actionIdentityEqual(existing, record) {
			return ActionRecord{}, fmt.Errorf("%w: %s", ErrActionConflict, record.ID)
		}
		return cloneActionRecord(existing), nil
	}
	if !errors.Is(err, ErrActionNotFound) {
		return ActionRecord{}, err
	}
	now := time.Now()
	record.State = ActionPrepared
	record.Authorization = ""
	record.PreparedAt = now
	record.AuthorizedAt = time.Time{}
	record.StartedAt = time.Time{}
	record.EndedAt = time.Time{}
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO actions(
			action_id, invocation_id, session_id, turn_id, tool_name, category, operation, arguments, fingerprint,
			metadata, preimages, cwd, paths, environment, network_intent, mcp_identity, policy_mode,
			state, authorization, prepared_at)
		VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
		record.ID, record.InvocationID, record.SessionID, record.TurnID,
		record.Tool, record.Category, record.Operation,
		record.Arguments, record.Fingerprint, record.Metadata, record.Preimages, record.CWD, paths, environment,
		record.NetworkIntent, record.MCPIdentity, record.PolicyMode,
		string(record.State), string(record.Authorization), now.UnixMilli()); err != nil {
		return ActionRecord{}, classifySQLiteError("insert action prepared", err)
	}
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO action_transitions(action_id, from_state, to_state, reason, timestamp)
		VALUES(?, ?, ?, ?, ?)`,
		record.ID, "", string(ActionPrepared), "action intent prepared", now.UnixMilli()); err != nil {
		return ActionRecord{}, classifySQLiteError("record action prepared transition", err)
	}
	if err := tx.Commit(); err != nil {
		return ActionRecord{}, classifySQLiteError("commit action prepared", err)
	}
	return cloneActionRecord(record), nil
}

func (s *SQLiteStore) AuthorizeAction(ctx context.Context, actionID, policyMode string) (ActionRecord, error) {
	return s.transitionAction(ctx, actionID, func(record *ActionRecord, now time.Time) error {
		if record.State == ActionAuthorized {
			if policyMode != "" && record.PolicyMode != policyMode {
				return fmt.Errorf("%w: policy mode changed", ErrActionConflict)
			}
			return nil
		}
		if record.State != ActionPrepared {
			return fmt.Errorf("%w: authorize action %q from %s", ErrActionState, actionID, record.State)
		}
		if policyMode != "" {
			record.PolicyMode = policyMode
		}
		record.Authorization = ActionAllow
		record.State = ActionAuthorized
		record.AuthorizedAt = now
		return nil
	})
}

func (s *SQLiteStore) DenyAction(ctx context.Context, actionID, reason string) (ActionRecord, error) {
	return s.transitionAction(ctx, actionID, func(record *ActionRecord, now time.Time) error {
		if record.State == ActionDenied {
			return nil
		}
		if record.State != ActionPrepared {
			return fmt.Errorf("%w: deny action %q from %s", ErrActionState, actionID, record.State)
		}
		record.Authorization = ActionDeny
		record.State = ActionDenied
		record.Error = reason
		record.EndedAt = now
		return nil
	})
}

func (s *SQLiteStore) StartAction(ctx context.Context, actionID, processIdentity string) (ActionRecord, error) {
	return s.transitionAction(ctx, actionID, func(record *ActionRecord, now time.Time) error {
		if record.State == ActionStarted {
			if record.ProcessIdentity != "" && processIdentity != "" && record.ProcessIdentity != processIdentity {
				return fmt.Errorf("%w: process identity changed", ErrActionConflict)
			}
			if record.ProcessIdentity == "" {
				record.ProcessIdentity = processIdentity
			}
			return nil
		}
		if record.State != ActionAuthorized {
			return fmt.Errorf("%w: start action %q from %s", ErrActionState, actionID, record.State)
		}
		record.State = ActionStarted
		record.StartedAt = now
		record.ProcessIdentity = processIdentity
		return nil
	})
}

func (s *SQLiteStore) FinishAction(ctx context.Context, actionID string, state ActionState, resultIdentity, reason, cleanup string) (ActionRecord, error) {
	if !validActionFinishState(state) {
		return ActionRecord{}, fmt.Errorf("%w: finish state %q is not terminal", ErrActionState, state)
	}
	return s.transitionAction(ctx, actionID, func(record *ActionRecord, now time.Time) error {
		if actionTerminal(record.State) {
			if record.State != state || record.ResultIdentity != resultIdentity || record.Error != reason || record.CleanupOutcome != cleanup {
				return fmt.Errorf("%w: action %q already finished as %s", ErrActionConflict, actionID, record.State)
			}
			return nil
		}
		if state == ActionCompleted || state == ActionIndeterminate {
			if record.State != ActionStarted {
				return fmt.Errorf("%w: finish action %q from %s", ErrActionState, actionID, record.State)
			}
		} else if state == ActionCancelled && record.State == ActionStarted {
			return fmt.Errorf("%w: cancellation crossed the start boundary for action %q", ErrActionState, actionID)
		} else if record.State != ActionPrepared && record.State != ActionAuthorized && record.State != ActionStarted {
			return fmt.Errorf("%w: finish action %q from %s", ErrActionState, actionID, record.State)
		}
		record.State = state
		record.ResultIdentity = resultIdentity
		record.Error = reason
		record.CleanupOutcome = cleanup
		record.EndedAt = now
		return nil
	})
}

// ReconcileAction is the only transition out of indeterminate. The caller
// must provide explicit verifier evidence; recovery and ordinary execution
// never infer that an unobserved effect did not happen.
func (s *SQLiteStore) ReconcileAction(ctx context.Context, actionID string, state ActionState, verification, resultIdentity, reason, cleanup string) (ActionRecord, error) {
	if !validActionFinishState(state) || state == ActionIndeterminate {
		return ActionRecord{}, fmt.Errorf("%w: reconcile state %q is not terminal", ErrActionState, state)
	}
	if strings.TrimSpace(verification) == "" {
		return ActionRecord{}, errors.New("reconciliation verification evidence is required")
	}
	return s.transitionAction(ctx, actionID, func(record *ActionRecord, _ time.Time) error {
		if record.State != ActionIndeterminate {
			return fmt.Errorf("%w: reconcile action %q from %s", ErrActionState, actionID, record.State)
		}
		record.State = state
		record.ResultIdentity = resultIdentity
		record.Error = strings.TrimSpace("verification: " + verification + "; " + reason)
		record.CleanupOutcome = cleanup
		record.EndedAt = time.Now()
		return nil
	})
}

// RecordActionRecovery persists host-side cleanup evidence while deliberately
// leaving an action indeterminate. A process can be terminated safely while
// its external effect remains unknown; only explicit user verification may
// move the action to a terminal outcome.
func (s *SQLiteStore) RecordActionRecovery(ctx context.Context, actionID, reason, cleanup string) (ActionRecord, error) {
	reason = strings.TrimSpace(reason)
	cleanup = strings.TrimSpace(cleanup)
	if reason == "" || cleanup == "" {
		return ActionRecord{}, errors.New("process recovery reason and cleanup outcome are required")
	}
	return s.transitionAction(ctx, actionID, func(record *ActionRecord, _ time.Time) error {
		if record.State != ActionIndeterminate {
			return fmt.Errorf("%w: recover action %q from %s", ErrActionState, actionID, record.State)
		}
		record.Error = appendActionEvidence(record.Error, reason)
		record.CleanupOutcome = appendActionEvidence(record.CleanupOutcome, cleanup)
		return nil
	})
}

func appendActionEvidence(existing, next string) string {
	if existing == "" {
		return next
	}
	if strings.Contains(existing, next) {
		return existing
	}
	return existing + "; " + next
}

func (s *SQLiteStore) transitionAction(ctx context.Context, actionID string, mutate func(*ActionRecord, time.Time) error) (ActionRecord, error) {
	ctx = normalizeContext(ctx)
	if actionID == "" {
		return ActionRecord{}, errors.New("action ID is required")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.ensureOpenLocked(); err != nil {
		return ActionRecord{}, err
	}
	tx, err := s.beginWriteLocked(ctx)
	if err != nil {
		return ActionRecord{}, err
	}
	defer tx.Rollback()
	record, err := actionRecordTx(ctx, tx, actionID)
	if err != nil {
		return ActionRecord{}, err
	}
	before := cloneActionRecord(record)
	if err := mutate(&record, time.Now()); err != nil {
		return ActionRecord{}, err
	}
	if record.State == before.State &&
		record.Authorization == before.Authorization &&
		record.PolicyMode == before.PolicyMode &&
		record.ProcessIdentity == before.ProcessIdentity &&
		record.ResultIdentity == before.ResultIdentity &&
		record.Error == before.Error &&
		record.CleanupOutcome == before.CleanupOutcome {
		return cloneActionRecord(record), nil
	}
	if _, err := tx.ExecContext(ctx, `
		UPDATE actions SET policy_mode = ?, state = ?, authorization = ?,
			result_identity = ?, error = ?, cleanup_outcome = ?, process_identity = ?,
			authorized_at = ?, started_at = ?, ended_at = ?
		WHERE action_id = ?`,
		record.PolicyMode, string(record.State), string(record.Authorization),
		record.ResultIdentity, record.Error, record.CleanupOutcome, record.ProcessIdentity,
		actionTimeMillis(record.AuthorizedAt), actionTimeMillis(record.StartedAt), actionTimeMillis(record.EndedAt),
		actionID); err != nil {
		return ActionRecord{}, classifySQLiteError("update action state", err)
	}
	if _, err := tx.ExecContext(ctx, `
		INSERT INTO action_transitions(action_id, from_state, to_state, reason, timestamp)
		VALUES(?, ?, ?, ?, ?)`,
		actionID, string(before.State), string(record.State), actionTransitionReason(before, record), time.Now().UnixMilli()); err != nil {
		return ActionRecord{}, classifySQLiteError("record action transition", err)
	}
	if err := tx.Commit(); err != nil {
		return ActionRecord{}, classifySQLiteError("commit action state", err)
	}
	return cloneActionRecord(record), nil
}

func actionTransitionReason(before, after ActionRecord) string {
	if before.State == after.State && (before.Error != after.Error || before.CleanupOutcome != after.CleanupOutcome) {
		return "action recovery evidence recorded"
	}
	if after.Error != "" {
		return after.Error
	}
	switch after.State {
	case ActionAuthorized:
		return "policy authorized exact action fingerprint"
	case ActionStarted:
		return "effect boundary durably acknowledged"
	case ActionCompleted:
		return "action completed"
	case ActionFailed:
		return "action failed"
	case ActionCancelled:
		return "action cancelled before a proven effect"
	case ActionDenied:
		return "action denied by policy"
	default:
		return fmt.Sprintf("action transitioned from %s", before.State)
	}
}

func actionTimeMillis(value time.Time) int64 {
	if value.IsZero() {
		return 0
	}
	return value.UnixMilli()
}

func (s *SQLiteStore) GetAction(ctx context.Context, actionID string) (ActionRecord, error) {
	ctx = normalizeContext(ctx)
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return ActionRecord{}, err
	}
	record, err := scanActionRecord(s.db.QueryRowContext(ctx, `
		SELECT action_id, invocation_id, session_id, turn_id, tool_name, category, operation, arguments, fingerprint,
		       metadata, preimages, cwd, paths, environment, network_intent, mcp_identity, policy_mode,
		       state, authorization, result_identity, error, cleanup_outcome,
		       process_identity, prepared_at, authorized_at, started_at, ended_at
		FROM actions WHERE action_id = ?`, actionID))
	if err == sql.ErrNoRows {
		return ActionRecord{}, fmt.Errorf("%w: %s", ErrActionNotFound, actionID)
	}
	if err != nil {
		return ActionRecord{}, fmt.Errorf("read action %q: %w", actionID, err)
	}
	return cloneActionRecord(record), nil
}

func (s *SQLiteStore) UnsettledActions(ctx context.Context) ([]ActionRecord, error) {
	ctx = normalizeContext(ctx)
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return nil, err
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT action_id, invocation_id, session_id, turn_id, tool_name, category, operation, arguments, fingerprint,
		       metadata, preimages, cwd, paths, environment, network_intent, mcp_identity, policy_mode,
		       state, authorization, result_identity, error, cleanup_outcome,
		       process_identity, prepared_at, authorized_at, started_at, ended_at
		FROM actions WHERE state IN (?, ?, ?, ?) ORDER BY prepared_at, action_id`,
		string(ActionPrepared), string(ActionAuthorized), string(ActionStarted), string(ActionIndeterminate))
	if err != nil {
		return nil, fmt.Errorf("list unsettled actions: %w", err)
	}
	defer rows.Close()
	var records []ActionRecord
	for rows.Next() {
		record, err := scanActionRecord(rows)
		if err != nil {
			return nil, fmt.Errorf("decode unsettled action: %w", err)
		}
		records = append(records, cloneActionRecord(record))
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return records, nil
}

func (s *SQLiteStore) ActionTransitions(ctx context.Context, actionID string) ([]ActionTransition, error) {
	ctx = normalizeContext(ctx)
	if actionID == "" {
		return nil, errors.New("action ID is required")
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	if err := s.ensureOpenLocked(); err != nil {
		return nil, err
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, action_id, from_state, to_state, reason, timestamp
		FROM action_transitions WHERE action_id = ? ORDER BY id`, actionID)
	if err != nil {
		return nil, fmt.Errorf("list action transitions: %w", err)
	}
	defer rows.Close()
	var transitions []ActionTransition
	for rows.Next() {
		var transition ActionTransition
		var timestamp int64
		if err := rows.Scan(&transition.ID, &transition.ActionID, &transition.From, &transition.To, &transition.Reason, &timestamp); err != nil {
			return nil, fmt.Errorf("decode action transition: %w", err)
		}
		transition.Timestamp = time.UnixMilli(timestamp)
		transitions = append(transitions, transition)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	if len(transitions) == 0 {
		return nil, fmt.Errorf("%w: %s", ErrActionNotFound, actionID)
	}
	return transitions, nil
}
