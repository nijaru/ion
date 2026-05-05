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

	"github.com/nijaru/canto/memory"
	"github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
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
