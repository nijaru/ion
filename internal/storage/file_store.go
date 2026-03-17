package storage

import (
	"bufio"
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/nijaru/ion/internal/session"
	"sync"
	_ "modernc.org/sqlite"
)

type fileStore struct {
	root   string
	dbs    map[string]*sql.DB
	inputs map[string]*sql.DB
	mu     sync.Mutex
}

func NewFileStore(root string) (Store, error) {
	if err := os.MkdirAll(root, 0755); err != nil {
		return nil, err
	}
	return &fileStore{
		root:   root,
		dbs:    make(map[string]*sql.DB),
		inputs: make(map[string]*sql.DB),
	}, nil
}

func (s *fileStore) OpenSession(ctx context.Context, cwd, model, branch string) (Session, error) {
	dir := s.dirFor(cwd)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return nil, err
	}

	id := fmt.Sprintf("%d-%s", time.Now().Unix(), session.ShortID())
	fileName := id + ".jsonl"
	path := filepath.Join(dir, fileName)

	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return nil, err
	}

	meta := Meta{
		Type:      "meta",
		ID:        id,
		CWD:       cwd,
		Model:     model,
		Branch:    branch,
		CreatedAt: time.Now().Unix(),
	}

	if err := json.NewEncoder(f).Encode(meta); err != nil {
		f.Close()
		return nil, err
	}

	// Initial index entry
	if err := s.updateIndex(dir, id, fileName, model, branch, meta.CreatedAt, 0, ""); err != nil {
		// Log error but continue
	}

	return &fileSession{
		store: s,
		f:     f,
		path:  path,
		meta:  meta,
	}, nil
}

func (s *fileStore) ResumeSession(ctx context.Context, id string) (Session, error) {
	var sessionPath string
	var meta Meta

	err := filepath.Walk(filepath.Join(s.root, "sessions"), func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if !info.IsDir() && info.Name() == id+".jsonl" {
			sessionPath = path
			return filepath.SkipAll
		}
		return nil
	})

	if err != nil {
		return nil, err
	}
	if sessionPath == "" {
		return nil, fmt.Errorf("session %s not found", id)
	}

	f, err := os.Open(sessionPath)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	if err := json.NewDecoder(f).Decode(&meta); err != nil {
		return nil, fmt.Errorf("failed to decode meta: %w", err)
	}

	// Re-open for append
	af, err := os.OpenFile(sessionPath, os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return nil, err
	}

	return &fileSession{
		store: s,
		f:     af,
		path:  sessionPath,
		meta:  meta,
	}, nil
}

func (s *fileStore) ListSessions(ctx context.Context, cwd string) ([]SessionInfo, error) {
	dir := s.dirFor(cwd)
	db, err := s.openIndexDB(dir)
	if err != nil {
		return nil, err
	}

	rows, err := db.QueryContext(ctx, "SELECT id, model, branch, created_at, updated_at, message_count, last_preview FROM sessions ORDER BY updated_at DESC")
	if err != nil {
		return nil, nil
	}
	defer rows.Close()

	var sessions []SessionInfo
	for rows.Next() {
		var si SessionInfo
		var ca, ua int64
		if err := rows.Scan(&si.ID, &si.Model, &si.Branch, &ca, &ua, &si.MessageCount, &si.LastPreview); err != nil {
			return nil, err
		}
		si.CreatedAt = time.Unix(ca, 0)
		si.UpdatedAt = time.Unix(ua, 0)
		sessions = append(sessions, si)
	}
	return sessions, nil
}

func (s *fileStore) GetRecentSession(ctx context.Context, cwd string) (*SessionInfo, error) {
	sessions, err := s.ListSessions(ctx, cwd)
	if err != nil || len(sessions) == 0 {
		return nil, err
	}
	return &sessions[0], nil
}

func (s *fileStore) AddInput(ctx context.Context, cwd, content string) error {
	dir := s.dirFor(cwd)
	db, err := s.openInputDB(dir)
	if err != nil {
		return err
	}

	_, err = db.ExecContext(ctx, "INSERT INTO inputs (content, created_at) VALUES (?, ?)", content, time.Now().Unix())
	return err
}

func (s *fileStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	dir := s.dirFor(cwd)
	db, err := s.openInputDB(dir)
	if err != nil {
		return nil, err
	}

	rows, err := db.QueryContext(ctx, "SELECT content FROM inputs ORDER BY created_at DESC LIMIT ?", limit)
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

func (s *fileStore) UpdateSession(ctx context.Context, si SessionInfo) error {
	// Not fully implemented as we need to resolve the directory for the ID
	return nil
}

func (s *fileStore) dirFor(cwd string) string {
	encoded := strings.ReplaceAll(cwd, string(filepath.Separator), "-")
	if encoded == "" {
		encoded = "root"
	}
	return filepath.Join(s.root, "sessions", encoded)
}

func (s *fileStore) updateIndex(dir, id, fileName, model, branch string, createdAt, updatedAt int64, preview string) error {
	db, err := s.openIndexDB(dir)
	if err != nil {
		return err
	}

	_, err = db.Exec(`
		INSERT INTO sessions (id, file_name, model, branch, created_at, updated_at, message_count, last_preview)
		VALUES (?, ?, ?, ?, ?, ?, 1, ?)
		ON CONFLICT(id) DO UPDATE SET
			updated_at = excluded.updated_at,
			message_count = sessions.message_count + 1,
			last_preview = CASE WHEN excluded.last_preview != '' THEN excluded.last_preview ELSE sessions.last_preview END
	`, id, fileName, model, branch, createdAt, updatedAt, preview)
	return err
}

func (s *fileStore) openIndexDB(dir string) (*sql.DB, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if db, ok := s.dbs[dir]; ok {
		return db, nil
	}

	if err := os.MkdirAll(dir, 0755); err != nil {
		return nil, err
	}

	db, err := sql.Open("sqlite", filepath.Join(dir, "index.db"))
	if err != nil {
		return nil, err
	}

	_, err = db.Exec(`
		CREATE TABLE IF NOT EXISTS sessions (
			id TEXT PRIMARY KEY,
			file_name TEXT NOT NULL,
			model TEXT,
			branch TEXT,
			name TEXT,
			created_at INTEGER NOT NULL,
			updated_at INTEGER NOT NULL,
			message_count INTEGER NOT NULL,
			last_preview TEXT
		);
		CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);
	`)
	if err != nil {
		db.Close()
		return nil, err
	}

	s.dbs[dir] = db
	return db, nil
}

func (s *fileStore) openInputDB(dir string) (*sql.DB, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if db, ok := s.inputs[dir]; ok {
		return db, nil
	}

	if err := os.MkdirAll(dir, 0755); err != nil {
		return nil, err
	}

	db, err := sql.Open("sqlite", filepath.Join(dir, "input.db"))
	if err != nil {
		return nil, err
	}

	_, err = db.Exec(`
		CREATE TABLE IF NOT EXISTS inputs (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			content TEXT NOT NULL,
			created_at INTEGER NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_inputs_created ON inputs(created_at DESC);
	`)
	if err != nil {
		db.Close()
		return nil, err
	}

	s.inputs[dir] = db
	return db, nil
}

type fileSession struct {
	store *fileStore
	f     *os.File
	path  string
	meta  Meta
	mu    sync.Mutex
}

func (s *fileSession) ID() string { return s.meta.ID }

func (s *fileSession) Meta() Metadata {
	return Metadata{
		ID:        s.meta.ID,
		CWD:       s.meta.CWD,
		Model:     s.meta.Model,
		Branch:    s.meta.Branch,
		CreatedAt: time.Unix(s.meta.CreatedAt, 0),
	}
}

func (s *fileSession) Append(ctx context.Context, event any) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := json.NewEncoder(s.f).Encode(event); err != nil {
		return err
	}

	// Update index if it's a message
	var preview string
	var isMsg bool

	switch e := event.(type) {
	case User:
		preview = e.Content
		isMsg = true
	case Assistant:
		for _, b := range e.Content {
			if b.Type == "text" && b.Text != nil {
				preview = *b.Text
				break
			}
		}
		isMsg = true
	}

	if isMsg {
		dir := filepath.Dir(s.path)
		s.store.updateIndex(dir, s.meta.ID, filepath.Base(s.path), s.meta.Model, s.meta.Branch, s.meta.CreatedAt, time.Now().Unix(), preview)
	}

	return nil
}

func (s *fileSession) Entries(ctx context.Context) ([]session.Entry, error) {
	f, err := os.Open(s.path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var entries []session.Entry
	scanner := bufio.NewScanner(f)
	// Skip meta line
	if scanner.Scan() {
		// Meta line skipped
	}

	for scanner.Scan() {
		line := scanner.Bytes()
		var raw map[string]any
		if err := json.Unmarshal(line, &raw); err != nil {
			continue
		}

		switch raw["type"] {
		case "user":
			var e User
			json.Unmarshal(line, &e)
			entries = append(entries, session.Entry{Role: session.User, Content: e.Content})
		case "assistant":
			var e Assistant
			json.Unmarshal(line, &e)
			var content strings.Builder
			for _, b := range e.Content {
				if b.Type == "text" && b.Text != nil {
					content.WriteString(*b.Text)
				}
			}
			entries = append(entries, session.Entry{Role: session.Assistant, Content: content.String()})
		case "tool_use":
			var e ToolUse
			json.Unmarshal(line, &e)
			entries = append(entries, session.Entry{Role: session.Tool, Title: e.Name})
		case "tool_result":
			var e ToolResult
			json.Unmarshal(line, &e)
			if len(entries) > 0 && entries[len(entries)-1].Role == session.Tool {
				entries[len(entries)-1].Content = e.Content
			} else {
				entries = append(entries, session.Entry{Role: session.Tool, Content: e.Content})
			}
		}
	}

	return entries, scanner.Err()
}

func (s *fileSession) Close() error {
	return s.f.Close()
}

func shortID() string {
	b := make([]byte, 4)
	if _, err := rand.Read(b); err != nil {
		return "abcd"
	}
	return hex.EncodeToString(b)
}
