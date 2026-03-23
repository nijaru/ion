package storage

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/go-json-experiment/json"
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
	dbPath := filepath.Join(root, "ion.db")

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
	_, err := s.db.ExecContext(ctx, "UPDATE session_meta SET updated_at = ?, last_preview = ? WHERE id = ?", 
		time.Now().Unix(), si.LastPreview, si.ID)
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
	// For now, we search globally across the store.
	items, err := s.memory.SearchKnowledge(ctx, query, limit)
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
		res = append(res, ki)
	}
	return res, nil
}

func (s *cantoStore) DeleteKnowledge(ctx context.Context, id string) error {
	return s.memory.DeleteKnowledge(ctx, id)
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
	// Map ion storage entries to Canto events if possible, or just ignore if CantoBackend is already saving them
	// Actually, when using CantoBackend, it will save its own events to the same SQLite store.
	// This Append method is used by the UI model to persist User inputs and Assistant responses
	// when NOT using Canto (e.g. in the old Native backend).
	
	// If we are using Canto, the CantoBackend will handle Appending to its own session.
	// But the UI still calls this.
	
	var preview string
	switch e := event.(type) {
	case User:
		preview = e.Content
		// We could save this to Canto store as a User message event
		ev := session.NewEvent(s.id, session.MessageAdded, llm.Message{
			Role:    llm.RoleUser,
			Content: e.Content,
		})
		s.store.canto.Save(ctx, ev)
	case Assistant:
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
		s.store.canto.Save(ctx, ev)
	}

	if preview != "" {
		s.store.UpdateSession(ctx, SessionInfo{ID: s.id, LastPreview: preview})
	}

	return nil
}

func (s *cantoSession) Entries(ctx context.Context) ([]ionsession.Entry, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return nil, err
	}

	var entries []ionsession.Entry
	for _, ev := range sess.Events() {
		switch ev.Type {
		case session.MessageAdded:
			var msg llm.Message
			if err := ev.UnmarshalData(&msg); err == nil {
				role := ionsession.Assistant
				if msg.Role == llm.RoleUser {
					role = ionsession.User
				}
				entries = append(entries, ionsession.Entry{
					Role:      role,
					Content:   msg.Content,
					Reasoning: msg.Reasoning,
				})
			}
		case session.ToolStarted:
			var data struct {
				Tool string `json:"tool"`
				Args string `json:"args"`
			}
			if err := ev.UnmarshalData(&data); err == nil {
				entries = append(entries, ionsession.Entry{
					Role:  ionsession.Tool,
					Title: fmt.Sprintf("%s(%s)", data.Tool, data.Args),
				})
			}
		case session.ToolCompleted:
			var data struct {
				Output string `json:"output"`
			}
			if err := ev.UnmarshalData(&data); err == nil {
				if len(entries) > 0 && entries[len(entries)-1].Role == ionsession.Tool {
					entries[len(entries)-1].Content = data.Output
				}
			}
		}
	}
	return entries, nil
}

func (s *cantoSession) Close() error {
	return nil
}
