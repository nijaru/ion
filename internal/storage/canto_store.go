package storage

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/memory"
	"github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/tooldisplay"
	_ "modernc.org/sqlite"
)

type cantoStore struct {
	dbPath string
	canto  *session.SQLiteStore
	memory *memory.CoreStore
	db     *sql.DB // Direct access for inputs and index

	mu sync.Mutex
}

const (
	ionSystemEvent   session.EventType = "ion_system"
	ionSubagentEvent session.EventType = "ion_subagent"
)

func NewCantoStore(root string) (Store, error) {
	if err := os.MkdirAll(root, 0o755); err != nil {
		return nil, err
	}
	dbPath := filepath.Join(root, "sessions.db")

	// Ensure all connections use WAL and busy_timeout
	dsn := dbPath + "?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)"

	cStore, err := session.NewSQLiteStore(dsn)
	if err != nil {
		return nil, err
	}

	mStore, err := memory.NewCoreStore(dsn)
	if err != nil {
		return nil, err
	}

	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}

	s := &cantoStore{
		dbPath: dbPath,
		canto:  cStore,
		memory: mStore,
		db:     db,
	}
	if err := s.init(); err != nil {
		return nil, err
	}

	return s, nil
}

func (s *cantoStore) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	var errs []error
	if err := s.canto.Close(); err != nil {
		errs = append(errs, err)
	}
	if err := s.memory.Close(); err != nil {
		errs = append(errs, err)
	}
	if err := s.db.Close(); err != nil {
		errs = append(errs, err)
	}
	if len(errs) > 0 {
		return fmt.Errorf("close errors: %v", errs)
	}
	return nil
}

func (s *cantoStore) init() error {
	queries := []string{
		`CREATE TABLE IF NOT EXISTS session_meta (
                        id TEXT PRIMARY KEY,
                        cwd TEXT,
                        model TEXT,
                        branch TEXT,
                        name TEXT,
                        created_at INTEGER,
                        updated_at INTEGER,
                        last_preview TEXT
                )`,
		`CREATE TABLE IF NOT EXISTS inputs (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        cwd TEXT,
                        content TEXT,
                        created_at INTEGER
                )`,
		`CREATE INDEX IF NOT EXISTS idx_inputs_cwd ON inputs(cwd)`,
		`CREATE INDEX IF NOT EXISTS idx_meta_cwd ON session_meta(cwd)`,
	}
	for _, q := range queries {
		if _, err := s.db.Exec(q); err != nil {
			return err
		}
	}
	if err := s.ensureColumn("session_meta", "name TEXT"); err != nil {
		return err
	}
	return nil
}

func (s *cantoStore) OpenSession(ctx context.Context, cwd, model, branch string) (Session, error) {
	id := fmt.Sprintf("%d-%s", time.Now().Unix(), ionsession.ShortID())

	_, err := s.db.ExecContext(
		ctx,
		"INSERT INTO session_meta (id, cwd, model, branch, name, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
		id,
		cwd,
		model,
		branch,
		"",
		time.Now().Unix(),
		time.Now().Unix(),
	)
	if err != nil {
		return nil, err
	}

	return &cantoSession{
		id:    id,
		store: s,
		meta: Meta{
			ID:        id,
			CWD:       cwd,
			Model:     model,
			Branch:    branch,
			CreatedAt: time.Now().Unix(),
		},
	}, nil
}

func (s *cantoStore) ResumeSession(ctx context.Context, id string) (Session, error) {
	var m Meta
	var ca int64
	err := s.db.QueryRowContext(ctx, "SELECT id, cwd, model, branch, created_at FROM session_meta WHERE id = ?", id).
		Scan(&m.ID, &m.CWD, &m.Model, &m.Branch, &ca)
	if err != nil {
		return nil, err
	}
	m.CreatedAt = ca

	return &cantoSession{
		id:    id,
		store: s,
		meta:  m,
	}, nil
}

func (s *cantoStore) ForkSession(
	ctx context.Context,
	parentID string,
	opts ForkOptions,
) (Session, error) {
	parent, err := s.sessionInfo(ctx, parentID)
	if err != nil {
		return nil, err
	}
	now := time.Now()
	childID := fmt.Sprintf("%d-%s", now.Unix(), ionsession.ShortID())
	_, err = s.canto.ForkWithOptions(ctx, parentID, childID, session.ForkOptions{
		BranchLabel: strings.TrimSpace(opts.Label),
		ForkReason:  strings.TrimSpace(opts.Reason),
	})
	if err != nil {
		return nil, err
	}

	title := strings.TrimSpace(opts.Label)
	if title == "" {
		title = "Fork of " + parent.ID
	}
	if _, err := s.db.ExecContext(
		ctx,
		`INSERT INTO session_meta
		 (id, cwd, model, branch, name, created_at, updated_at, last_preview)
		 VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
		childID,
		parent.CWD,
		parent.Model,
		parent.Branch,
		title,
		now.Unix(),
		now.Unix(),
		parent.LastPreview,
	); err != nil {
		return nil, err
	}

	return &cantoSession{
		id:    childID,
		store: s,
		meta: Meta{
			ID:        childID,
			CWD:       parent.CWD,
			Model:     parent.Model,
			Branch:    parent.Branch,
			CreatedAt: now.Unix(),
		},
	}, nil
}

func (s *cantoStore) sessionInfo(ctx context.Context, id string) (SessionInfo, error) {
	var si SessionInfo
	var ca, ua int64
	var title sql.NullString
	var preview sql.NullString
	err := s.db.QueryRowContext(
		ctx,
		`SELECT id, cwd, model, branch, created_at, updated_at, name, last_preview
		 FROM session_meta WHERE id = ?`,
		id,
	).Scan(
		&si.ID,
		&si.CWD,
		&si.Model,
		&si.Branch,
		&ca,
		&ua,
		&title,
		&preview,
	)
	if err != nil {
		return SessionInfo{}, err
	}
	si.CreatedAt = time.Unix(ca, 0)
	si.UpdatedAt = time.Unix(ua, 0)
	si.Title = title.String
	si.Summary = preview.String
	si.LastPreview = preview.String
	return si, nil
}

func (s *cantoStore) SessionTree(ctx context.Context, sessionID string) (SessionTree, error) {
	current, err := s.sessionInfo(ctx, sessionID)
	if err != nil {
		return SessionTree{}, err
	}
	lineageRecords, err := s.canto.Lineage(ctx, sessionID)
	if err != nil {
		return SessionTree{}, err
	}
	childrenRecords, err := s.canto.Children(ctx, sessionID)
	if err != nil {
		return SessionTree{}, err
	}
	tree := SessionTree{Current: current}
	tree.Lineage = make([]SessionInfo, 0, len(lineageRecords))
	for _, record := range lineageRecords {
		tree.Lineage = append(tree.Lineage, s.sessionInfoFromAncestry(ctx, record))
	}
	tree.Children = make([]SessionInfo, 0, len(childrenRecords))
	for _, record := range childrenRecords {
		tree.Children = append(tree.Children, s.sessionInfoFromAncestry(ctx, record))
	}
	return tree, nil
}

func (s *cantoStore) sessionInfoFromAncestry(
	ctx context.Context,
	record session.SessionAncestry,
) SessionInfo {
	info, err := s.sessionInfo(ctx, record.SessionID)
	if err == nil {
		return info
	}
	return SessionInfo{
		ID:        record.SessionID,
		Title:     record.BranchLabel,
		CreatedAt: record.CreatedAt,
		UpdatedAt: record.CreatedAt,
	}
}

func (s *cantoStore) ListSessions(ctx context.Context, cwd string) ([]SessionInfo, error) {
	rows, err := s.db.QueryContext(
		ctx,
		"SELECT id, model, branch, created_at, updated_at, name, last_preview FROM session_meta WHERE cwd = ? ORDER BY updated_at DESC",
		cwd,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var sessions []SessionInfo
	for rows.Next() {
		var si SessionInfo
		var ca, ua int64
		var title sql.NullString
		var preview sql.NullString
		if err := rows.Scan(&si.ID, &si.Model, &si.Branch, &ca, &ua, &title, &preview); err != nil {
			return nil, err
		}
		si.Title = title.String
		si.CreatedAt = time.Unix(ca, 0)
		si.UpdatedAt = time.Unix(ua, 0)
		si.Summary = preview.String
		si.LastPreview = preview.String
		// Note: MessageCount not easily available without querying events table
		sessions = append(sessions, si)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return sessions, nil
}

func (s *cantoStore) GetRecentSession(ctx context.Context, cwd string) (*SessionInfo, error) {
	sessions, err := s.ListSessions(ctx, cwd)
	if err != nil || len(sessions) == 0 {
		return nil, err
	}
	return &sessions[0], nil
}

func (s *cantoStore) AddInput(ctx context.Context, cwd, content string) error {
	_, err := s.db.ExecContext(
		ctx,
		"INSERT INTO inputs (cwd, content, created_at) VALUES (?, ?, ?)",
		cwd,
		content,
		time.Now().Unix(),
	)
	return err
}

func (s *cantoStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	rows, err := s.db.QueryContext(
		ctx,
		"SELECT content FROM inputs WHERE cwd = ? ORDER BY created_at DESC LIMIT ?",
		cwd,
		limit,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var inputs []string
	for rows.Next() {
		var content string
		if err := rows.Scan(&content); err != nil {
			return nil, err
		}
		inputs = append(inputs, content)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return inputs, nil
}

func (s *cantoStore) Canto() *session.SQLiteStore {
	return s.canto
}

func (s *cantoStore) CoreStore() *memory.CoreStore {
	return s.memory
}

func (s *cantoStore) UpdateSession(ctx context.Context, si SessionInfo) error {
	_, err := s.db.ExecContext(
		ctx,
		`UPDATE session_meta
		 SET updated_at = ?,
		     model = CASE WHEN ? != '' THEN ? ELSE model END,
		     branch = CASE WHEN ? != '' THEN ? ELSE branch END,
		     name = CASE WHEN (name IS NULL OR name = '') AND ? != '' THEN ? ELSE name END,
		     last_preview = CASE WHEN ? != '' THEN ? ELSE last_preview END
		 WHERE id = ?`,
		time.Now().Unix(),
		si.Model,
		si.Model,
		si.Branch,
		si.Branch,
		si.Title,
		si.Title,
		si.LastPreview,
		si.LastPreview,
		si.ID,
	)
	return err
}

func (s *cantoStore) ensureColumn(table, columnDef string) error {
	parts := strings.Fields(columnDef)
	if len(parts) == 0 {
		return nil
	}
	columnName := parts[0]

	rows, err := s.db.Query(fmt.Sprintf("PRAGMA table_info(%s)", table))
	if err != nil {
		return err
	}
	defer rows.Close()

	for rows.Next() {
		var (
			cid     int
			name    string
			ctype   string
			notnull int
			dflt    sql.NullString
			pk      int
		)
		if err := rows.Scan(&cid, &name, &ctype, &notnull, &dflt, &pk); err != nil {
			return err
		}
		if name == columnName {
			return nil
		}
	}

	_, err = s.db.Exec(fmt.Sprintf("ALTER TABLE %s ADD COLUMN %s", table, columnDef))
	return err
}

type cantoSession struct {
	id    string
	store *cantoStore
	meta  Meta
}

func (s *cantoSession) ID() string { return s.id }
func (s *cantoSession) Meta() Metadata {
	return Metadata{
		ID:        s.meta.ID,
		CWD:       s.meta.CWD,
		Model:     s.meta.Model,
		Branch:    s.meta.Branch,
		CreatedAt: time.Unix(s.meta.CreatedAt, 0),
	}
}

func (s *cantoSession) Append(ctx context.Context, event any) error {
	var title string
	var preview string
	var err error
	switch e := event.(type) {
	case User:
		return modelVisibleAppendError(event)
	case Agent:
		content, reasoning := agentMessagePayload(e)
		if !hasAgentMessagePayload(content, reasoning) {
			return nil
		}
		return modelVisibleAppendError(event)
	case Subagent:
		preview = sessionSummary(e.Content)
		ev := newStoredEvent(s.id, ionSubagentEvent, e, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case ToolUse:
		return modelVisibleAppendError(event)
	case ToolResult:
		return modelVisibleAppendError(event)
	case Status:
		if !isDurableResumeStatus(e.Status) {
			return nil
		}
		ev := newStoredEvent(s.id, session.EventType("status_changed"), map[string]any{
			"status": e.Status,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case System:
		preview = ""
		ev := newStoredEvent(s.id, ionSystemEvent, e, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case TokenUsage:
		ev := newStoredEvent(s.id, session.EventType("token_usage"), map[string]any{
			"input":  e.Input,
			"output": e.Output,
			"cost":   e.Cost,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case RoutingDecision:
		ev := newStoredEvent(s.id, session.EventType("routing_decision"), map[string]any{
			"decision":         e.Decision,
			"reason":           e.Reason,
			"model_slot":       e.ModelSlot,
			"provider":         e.Provider,
			"model":            e.Model,
			"reasoning":        e.Reasoning,
			"max_session_cost": e.MaxSessionCost,
			"max_turn_cost":    e.MaxTurnCost,
			"session_cost":     e.SessionCost,
			"turn_cost":        e.TurnCost,
			"stop_reason":      e.StopReason,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	case EscalationNotification:
		ev := newStoredEvent(s.id, session.EventType("escalation_notification"), map[string]any{
			"request_id": e.RequestID,
			"channel":    e.Channel,
			"target":     e.Target,
			"status":     e.Status,
			"detail":     e.Detail,
		}, e.TS)
		err = s.store.canto.Save(ctx, ev)
	default:
		return nil
	}

	if err != nil {
		return err
	}

	return s.store.UpdateSession(
		ctx,
		SessionInfo{ID: s.id, Title: title, Summary: preview, LastPreview: preview},
	)
}

func newStoredEvent(
	sessionID string,
	eventType session.EventType,
	data any,
	unixTS int64,
) session.Event {
	ev := session.NewEvent(sessionID, eventType, data)
	if unixTS > 0 {
		ev.Timestamp = time.Unix(unixTS, 0).UTC()
	}
	return ev
}

func modelVisibleAppendError(event any) error {
	return fmt.Errorf(
		"canto storage cannot append model-visible %T events; use the Canto runner",
		event,
	)
}

func (s *cantoSession) LastStatus(ctx context.Context) (string, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return "", err
	}

	var lastStatus string
	for _, ev := range sess.Events() {
		if ev.Type == session.EventType("status_changed") {
			var data struct {
				Status string `json:"status"`
			}
			if err := ev.UnmarshalData(&data); err != nil {
				continue
			}
			if isDurableResumeStatus(data.Status) {
				lastStatus = strings.TrimSpace(data.Status)
			} else {
				lastStatus = ""
			}
			continue
		}
		if lastStatus != "" && clearsDurableResumeStatus(ev.Type) {
			lastStatus = ""
		}
	}
	return lastStatus, nil
}

func isDurableResumeStatus(status string) bool {
	status = strings.TrimSpace(status)
	if status == "" {
		return false
	}
	return strings.Contains(strings.ToLower(status), "retrying")
}

func clearsDurableResumeStatus(eventType session.EventType) bool {
	switch eventType {
	case session.MessageAdded,
		session.ContextAdded,
		session.TurnCompleted,
		session.ToolCompleted,
		session.ApprovalResolved,
		session.ApprovalCanceled,
		session.CompactionTriggered,
		ionSystemEvent,
		ionSubagentEvent,
		session.EventType("token_usage"),
		session.EventType("routing_decision"),
		session.EventType("escalation_notification"):
		return true
	default:
		return false
	}
}

func (s *cantoSession) Entries(ctx context.Context) ([]ionsession.Entry, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return nil, err
	}

	history, err := sess.EffectiveEntries()
	if err != nil {
		return nil, err
	}

	entries := make([]ionsession.Entry, 0, len(history))
	effectiveByEventID := make(map[string]session.HistoryEntry, len(history))
	for _, entry := range history {
		if entry.EventID == "" {
			if display, ok := displayHistoryEntry(s.meta.CWD, entry); ok {
				entries = append(entries, display)
			}
			continue
		}
		effectiveByEventID[entry.EventID] = entry
	}

	seenEffective := make(map[string]bool, len(effectiveByEventID))
	for _, ev := range sess.Events() {
		if entry, ok := effectiveByEventID[ev.ID.String()]; ok {
			if display, ok := displayHistoryEntry(s.meta.CWD, entry); ok {
				display = withEntryTimestamp(display, ev.Timestamp)
				entries = append(entries, display)
			}
			seenEffective[entry.EventID] = true
			continue
		}
		if display, ok := displayEventEntry(ev); ok {
			display = withEntryTimestamp(display, ev.Timestamp)
			entries = append(entries, display)
		}
	}
	for _, entry := range history {
		if entry.EventID == "" || seenEffective[entry.EventID] {
			continue
		}
		if display, ok := displayHistoryEntry(s.meta.CWD, entry); ok {
			entries = append(entries, display)
		}
	}
	return normalizeDisplayEntries(entries), nil
}

func withEntryTimestamp(entry ionsession.Entry, timestamp time.Time) ionsession.Entry {
	if !timestamp.IsZero() {
		entry.Timestamp = timestamp.UTC()
	}
	return entry
}

func displayHistoryEntry(workdir string, entry session.HistoryEntry) (ionsession.Entry, bool) {
	if display, ok := displayContextEntry(entry); ok {
		return display, true
	}
	msg := entry.Message
	switch msg.Role {
	case llm.RoleUser:
		return ionsession.Entry{
			Role:    ionsession.User,
			Content: msg.Content,
		}, true
	case llm.RoleAssistant:
		return ionsession.Entry{
			Role:      ionsession.Agent,
			Content:   msg.Content,
			Reasoning: msg.Reasoning,
		}, true
	case llm.RoleTool:
		name := msg.Name
		args := ""
		isError := false
		if entry.Tool != nil {
			if entry.Tool.Name != "" {
				name = entry.Tool.Name
			}
			args = entry.Tool.Arguments
			isError = entry.Tool.IsError || strings.TrimSpace(entry.Tool.Error) != ""
		}
		title := tooldisplay.Title(name, args, tooldisplay.Options{Workdir: workdir})
		if title == "" {
			title = "tool"
		}
		return ionsession.Entry{
			Role:    ionsession.Tool,
			Title:   title,
			Content: msg.Content,
			IsError: isError,
		}, true
	case llm.RoleSystem, llm.RoleDeveloper:
		return ionsession.Entry{
			Role:    ionsession.System,
			Content: msg.Content,
		}, true
	default:
		return ionsession.Entry{}, false
	}
}

func normalizeDisplayEntries(entries []ionsession.Entry) []ionsession.Entry {
	normalized := make([]ionsession.Entry, 0, len(entries))
	for _, entry := range entries {
		if entry.Role == ionsession.Agent {
			if strings.TrimSpace(entry.Content) == "" && strings.TrimSpace(entry.Reasoning) == "" {
				continue
			}
		}
		normalized = append(normalized, entry)
	}
	return normalized
}

func displayContextEntry(entry session.HistoryEntry) (ionsession.Entry, bool) {
	if entry.EventType != session.ContextAdded {
		return ionsession.Entry{}, false
	}
	switch entry.ContextKind {
	case session.ContextKindSummary, session.ContextKindWorkingSet, session.ContextKindBootstrap:
		return ionsession.Entry{
			Role:    ionsession.System,
			Content: entry.Message.Content,
		}, true
	default:
		return ionsession.Entry{}, false
	}
}

func displayEventEntry(ev session.Event) (ionsession.Entry, bool) {
	switch ev.Type {
	case ionSystemEvent:
		var data System
		if err := ev.UnmarshalData(&data); err != nil {
			return ionsession.Entry{}, false
		}
		return ionsession.Entry{
			Role:    ionsession.System,
			Content: data.Content,
		}, true
	case ionSubagentEvent:
		var data Subagent
		if err := ev.UnmarshalData(&data); err != nil {
			return ionsession.Entry{}, false
		}
		return ionsession.Entry{
			Role:    ionsession.Subagent,
			Title:   data.Name,
			Content: data.Content,
			IsError: data.IsError,
		}, true
	default:
		return ionsession.Entry{}, false
	}
}

func (s *cantoSession) Usage(ctx context.Context) (int, int, float64, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return 0, 0, 0, err
	}

	var input, output int
	var cost float64

	for _, ev := range sess.Events() {
		// Use literal string for TokenUsage event type
		if ev.Type == "token_usage" {
			var data struct {
				Input  int     `json:"input"`
				Output int     `json:"output"`
				Cost   float64 `json:"cost"`
			}
			if err := ev.UnmarshalData(&data); err == nil {
				input += data.Input
				output += data.Output
				cost += data.Cost
			}
		}
	}

	return input, output, cost, nil
}

func (s *cantoSession) Close() error {
	return nil
}
