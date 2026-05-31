package storage

import (
	"context"
	"encoding/json"
	"strings"
	"time"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/llm"
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

	// Close finalizes and closes any open storage resources.
	Close() error
}

type ForkOptions struct {
	Label  string
	Reason string
}

type SessionForker interface {
	ForkSession(ctx context.Context, parentID string, opts ForkOptions) (Session, error)
}

type SessionTree struct {
	Current  SessionInfo
	Lineage  []SessionInfo
	Children []SessionInfo
}

type SessionTreeReader interface {
	SessionTree(ctx context.Context, sessionID string) (SessionTree, error)
}

type SessionBundle struct {
	Version       int                   `json:"version"`
	ExportedAt    time.Time             `json:"exported_at"`
	RootSessionID string                `json:"root_session_id"`
	Sessions      []SessionBundleRecord `json:"sessions"`
	Checksum      string                `json:"checksum"`
}

type SessionBundleRecord struct {
	Info          SessionInfo         `json:"info"`
	Ancestry      SessionAncestryInfo `json:"ancestry"`
	Events        []json.RawMessage   `json:"events"`
	EventCount    int                 `json:"event_count"`
	EventChecksum string              `json:"event_checksum"`
}

type SessionAncestryInfo struct {
	SessionID        string    `json:"session_id"`
	ParentSessionID  string    `json:"parent_session_id,omitempty"`
	ForkPointEventID string    `json:"fork_point_event_id,omitempty"`
	BranchLabel      string    `json:"branch_label,omitempty"`
	ForkReason       string    `json:"fork_reason,omitempty"`
	Depth            int       `json:"depth"`
	CreatedAt        time.Time `json:"created_at"`
}

type SessionBundleExporter interface {
	ExportSessionBundle(ctx context.Context, sessionID string) (SessionBundle, error)
}

type SessionBundleImporter interface {
	ImportSessionBundle(ctx context.Context, bundle SessionBundle) ([]SessionInfo, error)
}

// Session handles appending events to a specific session's storage.
type Session interface {
	// ID returns the unique session identifier.
	ID() string

	// Meta returns the session's initial metadata.
	Meta() Metadata

	// Append appends an Ion-owned display/progress event to the session storage.
	Append(ctx context.Context, event Event) error

	// AppendModelMessage appends a provider-visible message to durable history.
	AppendModelMessage(ctx context.Context, message llm.Message) error

	// ModelMessages returns the effective provider-visible message history.
	ModelMessages(ctx context.Context) ([]llm.Message, error)

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
	CWD          string    `json:"cwd"`
	Model        string    `json:"model"`
	Branch       string    `json:"branch"`
	CreatedAt    time.Time `json:"created_at"`
	UpdatedAt    time.Time `json:"updated_at"`
	MessageCount int       `json:"message_count"`
	Title        string    `json:"title"`
	// Summary is a read-side alias for LastPreview. Persistent metadata stores
	// one preview string so session lists, bundles, and pickers cannot diverge.
	Summary           string `json:"summary"`
	LastPreview       string `json:"last_preview"`
	PreserveUpdatedAt bool   `json:"-"`
}

func IsConversationSessionInfo(info SessionInfo) bool {
	title := strings.TrimSpace(info.Title)
	if strings.HasPrefix(title, "/") {
		return false
	}
	preview := strings.TrimSpace(info.LastPreview)
	return preview != "" && !strings.HasPrefix(preview, "/")
}

// Event is a sealed storage event. Provider-visible messages use
// AppendModelMessage instead of this display/progress append path.
type Event interface {
	isStorageEvent()
}

// Event projection types persisted into Canto for Ion-owned display/session state.
type (
	Meta struct {
		Type      string `json:"type"` // "meta"
		ID        string `json:"id"`
		CWD       string `json:"cwd"`
		Model     string `json:"model"`
		Branch    string `json:"branch"`
		CreatedAt int64  `json:"created_at"`
	}

	Status struct {
		Type   string `json:"type"` // "status"
		Status string `json:"status"`
		TS     int64  `json:"ts"`
	}

	System struct {
		Type    string `json:"type"` // "system"
		Content string `json:"content"`
		TS      int64  `json:"ts"`
	}

	TokenUsage struct {
		Type   string  `json:"type"` // "token_usage"
		Input  int     `json:"input"`
		Output int     `json:"output"`
		Cost   float64 `json:"cost"`
		TS     int64   `json:"ts"`
	}

	RoutingDecision struct {
		Type           string  `json:"type"` // "routing_decision"
		Decision       string  `json:"decision"`
		Reason         string  `json:"reason"`
		ModelSlot      string  `json:"model_slot"`
		Provider       string  `json:"provider"`
		Model          string  `json:"model"`
		Reasoning      string  `json:"reasoning,omitempty"`
		MaxSessionCost float64 `json:"max_session_cost,omitempty"`
		MaxTurnCost    float64 `json:"max_turn_cost,omitempty"`
		SessionCost    float64 `json:"session_cost,omitempty"`
		TurnCost       float64 `json:"turn_cost,omitempty"`
		StopReason     string  `json:"stop_reason,omitempty"`
		TS             int64   `json:"ts"`
	}

	Subagent struct {
		Type    string `json:"type"` // "subagent"
		Name    string `json:"name"`
		Content string `json:"content"`
		IsError bool   `json:"is_error"`
		TS      int64  `json:"ts"`
	}
)

func (Status) isStorageEvent()          {}
func (System) isStorageEvent()          {}
func (TokenUsage) isStorageEvent()      {}
func (RoutingDecision) isStorageEvent() {}
func (Subagent) isStorageEvent()        {}

func sessionTitle(text string) string {
	return compactSessionText(text, 72)
}

func sessionSummary(text string) string {
	return compactSessionText(text, 120)
}

func sessionInfoPreview(info SessionInfo) string {
	if preview := strings.TrimSpace(info.LastPreview); preview != "" {
		return preview
	}
	return strings.TrimSpace(info.Summary)
}

func compactSessionText(text string, max int) string {
	if max <= 0 {
		return ""
	}
	text = strings.TrimSpace(text)
	if text == "" {
		return ""
	}
	text = strings.NewReplacer("\r", " ", "\n", " ").Replace(text)
	text = strings.Join(strings.Fields(text), " ")
	if len(text) <= max {
		return text
	}
	if max <= 1 {
		return text[:max]
	}
	return text[:max-1] + "…"
}
