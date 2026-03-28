package storage

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/nijaru/canto/llm"
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
}

func NewCantoStore(root string) (Store, error) {
	if err := os.MkdirAll(root, 0755); err != nil {
		return nil, err
	}
	dbPath := filepath.Join(root, "sessions.db")

	cStore, err := session.NewSQLiteStore(dbPath)
	if err != nil {
		return nil, err
	}

	mStore, err := memory.NewCoreStore(dbPath)
	if err != nil {
		return nil, err
	}

	db, err := sql.Open("sqlite", dbPath+"?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)")
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

func (s *cantoStore) init() error {
	queries := []string{
		`CREATE TABLE IF NOT EXISTS session_meta (
                        id TEXT PRIMARY KEY,
                        cwd TEXT,
                        model TEXT,
                        branch TEXT,
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
	return nil
}
func (s *cantoStore) OpenSession(ctx context.Context, cwd, model, branch string) (Session, error) {
	id := fmt.Sprintf("%d-%s", time.Now().Unix(), ionsession.ShortID())

	_, err := s.db.ExecContext(ctx,
		"INSERT INTO session_meta (id, cwd, model, branch, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
		id, cwd, model, branch, time.Now().Unix(), time.Now().Unix())
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

func (s *cantoStore) ListSessions(ctx context.Context, cwd string) ([]SessionInfo, error) {
	rows, err := s.db.QueryContext(ctx,
		"SELECT id, model, branch, created_at, updated_at, last_preview FROM session_meta WHERE cwd = ? ORDER BY updated_at DESC", cwd)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var sessions []SessionInfo
	for rows.Next() {
		var si SessionInfo
		var ca, ua int64
		var preview sql.NullString
		if err := rows.Scan(&si.ID, &si.Model, &si.Branch, &ca, &ua, &preview); err != nil {
			return nil, err
		}
		si.CreatedAt = time.Unix(ca, 0)
		si.UpdatedAt = time.Unix(ua, 0)
		si.LastPreview = preview.String
		// Note: MessageCount not easily available without querying events table
		sessions = append(sessions, si)
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
	_, err := s.db.ExecContext(ctx, "INSERT INTO inputs (cwd, content, created_at) VALUES (?, ?, ?)", cwd, content, time.Now().Unix())
	return err
}

func (s *cantoStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	rows, err := s.db.QueryContext(ctx, "SELECT content FROM inputs WHERE cwd = ? ORDER BY created_at DESC LIMIT ?", cwd, limit)
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
		     last_preview = CASE WHEN ? != '' THEN ? ELSE last_preview END
		 WHERE id = ?`,
		time.Now().Unix(),
		si.Model,
		si.Model,
		si.Branch,
		si.Branch,
		si.LastPreview,
		si.LastPreview,
		si.ID,
	)
	return err
}

func (s *cantoStore) SaveKnowledge(ctx context.Context, item KnowledgeItem) error {
	metadata := item.Metadata
	if metadata == nil {
		metadata = make(map[string]any)
	}
	metadata["cwd"] = item.CWD
	if item.Path != "" {
		metadata["path"] = item.Path
	}

	return s.memory.SaveKnowledge(ctx, &memory.KnowledgeItem{
		ID:       item.ID,
		Content:  item.Content,
		Metadata: metadata,
	})
}

func (s *cantoStore) SearchKnowledge(ctx context.Context, cwd, query string, limit int) ([]KnowledgeItem, error) {
	// Note: Canto's SearchKnowledge currently doesn't filter by CWD in its core table,
	// so we might need to filter manually if necessary, or update Canto.
	// Fetch a little extra and filter locally so workspace-specific recall stays scoped.
	searchLimit := limit
	if searchLimit > 0 && cwd != "" {
		searchLimit *= 5
	}
	items, err := s.memory.SearchKnowledge(ctx, query, searchLimit)
	if err != nil {
		return nil, err
	}

	res := make([]KnowledgeItem, 0, len(items))
	for _, item := range items {
		ki := KnowledgeItem{
			ID:       item.ID,
			Content:  item.Content,
			Metadata: item.Metadata,
		}
		if cwdStr, ok := item.Metadata["cwd"].(string); ok {
			ki.CWD = cwdStr
		}
		if pathStr, ok := item.Metadata["path"].(string); ok {
			ki.Path = pathStr
		}
		if cwd != "" && ki.CWD != "" && ki.CWD != cwd {
			continue
		}
		res = append(res, ki)
		if limit > 0 && len(res) >= limit {
			break
		}
	}
	return res, nil
}

func (s *cantoStore) DeleteKnowledge(ctx context.Context, id string) error {
	return fmt.Errorf("knowledge deletion is not supported by the canto store")
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
	var preview string
	var err error
	switch e := event.(type) {
	case User:
		preview = e.Content
		ev := session.NewEvent(s.id, session.MessageAdded, llm.Message{
			Role:    llm.RoleUser,
			Content: e.Content,
		})
		err = s.store.canto.Save(ctx, ev)
	case Agent:
		var content strings.Builder
		var reasoning strings.Builder
		for _, b := range e.Content {
			if b.Type == "text" && b.Text != nil {
				content.WriteString(*b.Text)
			}
			if b.Type == "thinking" && b.Thinking != nil {
				reasoning.WriteString(*b.Thinking)
			}
		}
		preview = content.String()
		ev := session.NewEvent(s.id, session.MessageAdded, llm.Message{
			Role:      llm.RoleAssistant,
			Content:   preview,
			Reasoning: reasoning.String(),
		})
		err = s.store.canto.Save(ctx, ev)
	case ToolUse:
		ev := session.NewEvent(s.id, session.ToolStarted, map[string]any{
			"id":   e.ID,
			"tool": e.Name,
			"args": e.Input,
		})
		err = s.store.canto.Save(ctx, ev)
	case ToolResult:
		completed := session.NewEvent(s.id, session.ToolCompleted, map[string]any{
			"tool_use_id": e.ToolUseID,
			"output":      e.Content,
			"is_error":    e.IsError,
		})
		if err = s.store.canto.Save(ctx, completed); err != nil {
			break
		}
		toolName, lookupErr := s.toolNameForUseID(ctx, e.ToolUseID)
		if lookupErr != nil {
			err = lookupErr
			break
		}
		preview = e.Content
		msg := llm.Message{
			Role:    llm.RoleTool,
			Content: e.Content,
			Name:    toolName,
			ToolID:  e.ToolUseID,
		}
		err = s.store.canto.Save(ctx, session.NewEvent(s.id, session.MessageAdded, msg))
	case Status:
		ev := session.NewEvent(s.id, session.EventType("status_changed"), map[string]any{
			"status": e.Status,
		})
		err = s.store.canto.Save(ctx, ev)
	case TokenUsage:
		ev := session.NewEvent(s.id, session.EventType("token_usage"), map[string]any{
			"input":  e.Input,
			"output": e.Output,
			"cost":   e.Cost,
		})
		err = s.store.canto.Save(ctx, ev)
	default:
		return nil
	}

	if err != nil {
		return err
	}

	return s.store.UpdateSession(ctx, SessionInfo{ID: s.id, LastPreview: preview})
}

func (s *cantoSession) toolNameForUseID(ctx context.Context, toolUseID string) (string, error) {
	if toolUseID == "" {
		return "", nil
	}
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return "", err
	}
	for i := len(sess.Events()) - 1; i >= 0; i-- {
		ev := sess.Events()[i]
		if ev.Type != session.ToolStarted {
			continue
		}
		var data struct {
			ID   string `json:"id"`
			Tool string `json:"tool"`
		}
		if err := ev.UnmarshalData(&data); err != nil {
			return "", err
		}
		if data.ID == toolUseID {
			return data.Tool, nil
		}
	}
	return "", nil
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
			if err := ev.UnmarshalData(&data); err == nil {
				lastStatus = data.Status
			}
		}
	}
	return lastStatus, nil
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
	for _, entry := range history {
		msg := entry.Message
		switch msg.Role {
		case llm.RoleUser:
			entries = append(entries, ionsession.Entry{
				Role:    ionsession.User,
				Content: msg.Content,
			})
		case llm.RoleAssistant:
			entries = append(entries, ionsession.Entry{
				Role:      ionsession.Agent,
				Content:   msg.Content,
				Reasoning: msg.Reasoning,
			})
		case llm.RoleTool:
			title := msg.Name
			if title == "" {
				title = "tool"
			}
			entries = append(entries, ionsession.Entry{
				Role:    ionsession.Tool,
				Title:   title,
				Content: msg.Content,
			})
		case llm.RoleSystem, llm.RoleDeveloper:
			entries = append(entries, ionsession.Entry{
				Role:    ionsession.System,
				Content: msg.Content,
			})
		}
	}
	return entries, nil
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
