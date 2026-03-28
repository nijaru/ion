package storage

import (
	"context"
	"time"

	"github.com/nijaru/canto/memory"
	"github.com/nijaru/ion/internal/session"
)

// Store defines the interface for session and input history persistence.
type Store interface {
	// OpenSession initializes a new session and its storage file.
	OpenSession(ctx context.Context, cwd, model, branch string) (Session, error)

	// ResumeSession loads an existing session by ID.
	ResumeSession(ctx context.Context, id string) (Session, error)

	// ListSessions returns a list of sessions for the given working directory.
	ListSessions(ctx context.Context, cwd string) ([]SessionInfo, error)

	// GetRecentSession returns the most recently updated session for the given directory.
	GetRecentSession(ctx context.Context, cwd string) (*SessionInfo, error)

	// AddInput appends a user input string to the directory's history.
	AddInput(ctx context.Context, cwd, content string) error

	// GetInputs returns the input history for the given directory.
	GetInputs(ctx context.Context, cwd string, limit int) ([]string, error)

	// UpdateSession updates the session's index metadata.
	UpdateSession(ctx context.Context, si SessionInfo) error

	// SaveKnowledge persists a knowledge item for cross-session recall.
	SaveKnowledge(ctx context.Context, item KnowledgeItem) error

	// SearchKnowledge finds items matching the query for the given directory.
	SearchKnowledge(ctx context.Context, cwd, query string, limit int) ([]KnowledgeItem, error)

	// DeleteKnowledge removes a specific item by ID.
	DeleteKnowledge(ctx context.Context, id string) error

	// CoreStore returns the underlying Canto memory store.
	CoreStore() *memory.CoreStore
}

// Session handles appending events to a specific session's storage.
type Session interface {
	// ID returns the unique session identifier.
	ID() string

	// Meta returns the session's initial metadata.
	Meta() Metadata

	// Append appends a new event to the session storage.
	Append(ctx context.Context, event any) error

	// Entries returns all entries stored in the session so far.
	Entries(ctx context.Context) ([]session.Entry, error)

	// LastStatus returns the most recent status persisted in the session.
	LastStatus(ctx context.Context) (string, error)

	// Usage returns the total tokens and cost accumulated in the session.
	Usage(ctx context.Context) (int, int, float64, error)

	// Close finalizes and closes any open file handles for the session.
	Close() error
}

// Metadata represents the persistent meta for a session.
type Metadata struct {
	ID        string    `json:"id"`
	CWD       string    `json:"cwd"`
	Model     string    `json:"model"`
	Branch    string    `json:"branch"`
	CreatedAt time.Time `json:"created_at"`
}

// SessionInfo provides summary information about a session for lists and pickers.
type SessionInfo struct {
	ID           string    `json:"id"`
	Model        string    `json:"model"`
	Branch       string    `json:"branch"`
	CreatedAt    time.Time `json:"created_at"`
	UpdatedAt    time.Time `json:"updated_at"`
	MessageCount int       `json:"message_count"`
	LastPreview  string    `json:"last_preview"`
}

// KnowledgeItem represents a piece of information stored in long-term memory.
// It can be a code fragment, a file summary, or a cross-session insight.
type KnowledgeItem struct {
	ID        string         `json:"id"`
	CWD       string         `json:"cwd"`
	Path      string         `json:"path,omitzero"`
	Content   string         `json:"content"`
	Metadata  map[string]any `json:"metadata,omitzero"`
	UpdatedAt time.Time      `json:"updated_at"`
}

// JSONL entry types as defined in ai/design/session-storage.md
type (
	Meta struct {
		Type      string `json:"type"` // "meta"
		ID        string `json:"id"`
		CWD       string `json:"cwd"`
		Model     string `json:"model"`
		Branch    string `json:"branch"`
		CreatedAt int64  `json:"created_at"`
	}

	User struct {
		Type    string `json:"type"` // "user"
		Content string `json:"content"`
		TS      int64  `json:"ts"`
	}

	Status struct {
		Type   string `json:"type"` // "status"
		Status string `json:"status"`
		TS     int64  `json:"ts"`
	}

	Agent struct {
		Type    string  `json:"type"` // "agent"
		Content []Block `json:"content"`
		TS      int64   `json:"ts"`
	}

	ToolUse struct {
		Type  string `json:"type"` // "tool_use"
		ID    string `json:"id"`
		Name  string `json:"name"`
		Input any    `json:"input"`
		TS    int64  `json:"ts"`
	}

	ToolResult struct {
		Type      string `json:"type"` // "tool_result"
		ToolUseID string `json:"tool_use_id"`
		Content   string `json:"content"`
		IsError   bool   `json:"is_error"`
		TS        int64  `json:"ts"`
	}

	TokenUsage struct {
		Type   string  `json:"type"` // "token_usage"
		Input  int     `json:"input"`
		Output int     `json:"output"`
		Cost   float64 `json:"cost"`
		TS     int64   `json:"ts"`
	}

	Block struct {
		Type     string  `json:"type"` // "text" or "thinking"
		Text     *string `json:"text,omitempty"`
		Thinking *string `json:"thinking,omitempty"`
	}
)
